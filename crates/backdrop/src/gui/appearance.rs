//! Light/dark appearance and color themes: applying the configured themes,
//! following the desktop's appearance, reloading theme files when they
//! change, and the Appearance group of the settings page.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use backdrop_core::settings::{Appearance, Settings};
use futures::StreamExt as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::group_box::GroupBox;
use gpui_kit::component::label::Label;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, IndexPath, Theme, ThemeConfig, ThemeMode, ThemeRegistry, WindowExt as _,
    v_flex,
};
use gpui_kit::*;
use notify::Watcher as _;

use super::settings_page::setting_row;
use super::themes::Themes;

/// Saving a file often produces several events in quick succession, and the
/// file can be incomplete in between. Changes are applied once this long
/// after the last event.
const RELOAD_DELAY: Duration = Duration::from_millis(200);

const CHOICES: [(Appearance, &str); 3] = [
    (Appearance::System, "System"),
    (Appearance::Light, "Light"),
    (Appearance::Dark, "Dark"),
];

type ThemeSelect = Entity<SelectState<Vec<SharedString>>>;

/// The appearance settings, applied to the app's theme. Created with the
/// window and kept for its lifetime, so the theme follows the desktop and
/// the themes folder even while the settings page is closed.
pub struct AppearanceSettings {
    settings_file: PathBuf,
    appearance: Appearance,
    light_theme: Option<String>,
    dark_theme: Option<String>,
    themes: Themes,
    /// Why the configured light and dark themes are not in use, if so.
    problems: [Option<String>; 2],
    /// Problems last shown as notifications, so each one appears once.
    reported: HashSet<String>,
    light_select: ThemeSelect,
    dark_select: ThemeSelect,
    /// The latest settings save; each one waits for the one before it, so
    /// changes land in the order they were made.
    save_task: Option<Task<()>>,
    _watcher: Option<notify::RecommendedWatcher>,
    _subscriptions: Vec<Subscription>,
}

fn default_themes(cx: &App) -> [Rc<ThemeConfig>; 2] {
    let registry = ThemeRegistry::global(cx);
    [
        registry.default_light_theme().clone(),
        registry.default_dark_theme().clone(),
    ]
}

impl AppearanceSettings {
    /// Reads the settings and themes and applies them. Problems are reported
    /// once the window can show them.
    pub fn new(
        settings_file: PathBuf,
        themes_dir: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Read here rather than in the background, so the first frame already
        // has the right theme. An unreadable file leaves the defaults; the
        // workspace reports it, and saving refuses to replace it.
        let settings = Settings::load(&settings_file).unwrap_or_default();
        let new_select = |mode: ThemeMode, window: &mut Window, cx: &mut Context<Self>| {
            let select = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx));
            let subscription = cx.subscribe_in(
                &select,
                window,
                move |this, _, event: &SelectEvent<Vec<SharedString>>, window, cx| {
                    if let SelectEvent::Confirm(Some(name)) = event {
                        this.set_theme(mode, name, window, cx);
                    }
                },
            );
            (select, subscription)
        };
        let (light_select, light_subscription) = new_select(ThemeMode::Light, window, cx);
        let (dark_select, dark_subscription) = new_select(ThemeMode::Dark, window, cx);
        let appearance = cx.observe_window_appearance(window, |this, window, cx| {
            if this.appearance == Appearance::System {
                Theme::change(window.appearance(), Some(window), cx);
            }
        });
        let mut this = Self {
            settings_file,
            appearance: settings.appearance(),
            light_theme: settings.light_theme().map(str::to_owned),
            dark_theme: settings.dark_theme().map(str::to_owned),
            themes: Themes::load(themes_dir, default_themes(cx)),
            problems: [None, None],
            reported: HashSet::new(),
            light_select,
            dark_select,
            save_task: None,
            _watcher: Self::watch_themes(themes_dir, window, cx),
            _subscriptions: vec![light_subscription, dark_subscription, appearance],
        };
        this.apply_themes(window, cx);
        // Notifications need the window's root, which exists only once the
        // window's view is built; a task runs after that.
        cx.spawn_in(window, async move |this, cx| {
            this.update_in(cx, |this, window, cx| {
                this.report_theme_problems(window, cx)
            })
            .ok();
        })
        .detach();
        this
    }

    /// Calls [`Self::reload_themes`] whenever a file in `dir` changes. On
    /// failure the themes are still loaded at startup, just not reloaded.
    fn watch_themes(
        dir: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<notify::RecommendedWatcher> {
        // The test scheduler fails a test when another thread wakes one of its
        // tasks, as the watcher's thread would; tests reload directly.
        if cfg!(test) {
            return None;
        }
        // The folder is watched rather than each file, because many editors
        // save by replacing the file. It must exist to be watched.
        if let Err(error) = std::fs::create_dir_all(dir) {
            eprintln!("{}: {error}; theme changes need a restart", dir.display());
            return None;
        }
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if event.is_ok_and(|event| !event.kind.is_access()) {
                tx.unbounded_send(()).ok();
            }
        })
        .and_then(|mut watcher| {
            watcher.watch(dir, notify::RecursiveMode::NonRecursive)?;
            Ok(watcher)
        });
        let watcher = match watcher {
            Ok(watcher) => watcher,
            Err(error) => {
                eprintln!("{}: {error}; theme changes need a restart", dir.display());
                return None;
            }
        };
        cx.spawn_in(window, async move |this, cx| {
            while rx.next().await.is_some() {
                // Wait for the burst of events from one save to settle.
                loop {
                    cx.background_executor().timer(RELOAD_DELAY).await;
                    let mut more = false;
                    while rx.try_recv().is_ok() {
                        more = true;
                    }
                    if !more {
                        break;
                    }
                }
                if this
                    .update_in(cx, |this, window, cx| this.reload_themes(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Some(watcher)
    }

    fn reload_themes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.themes.dir().to_owned();
        self.themes = Themes::load(&dir, default_themes(cx));
        self.apply_themes(window, cx);
        self.sync_selects(window, cx);
        self.report_theme_problems(window, cx);
    }

    /// The mode the app is in now: the configured one, or the desktop's.
    fn theme_mode(&self, window: &Window) -> ThemeMode {
        match self.appearance {
            Appearance::System => window.appearance().into(),
            Appearance::Light => ThemeMode::Light,
            Appearance::Dark => ThemeMode::Dark,
        }
    }

    /// Resolves the configured light and dark themes, falling back to the
    /// defaults, and makes them the active ones.
    fn apply_themes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.problems = [None, None];
        let themes = &self.themes;
        let mut resolve = |mode: ThemeMode, name: Option<&str>| {
            themes.resolve(name, mode).unwrap_or_else(|problem| {
                let default = themes.default_for(mode);
                self.problems[usize::from(mode.is_dark())] =
                    Some(format!("{problem} Using {} instead.", default.name));
                default.clone()
            })
        };
        let light = resolve(ThemeMode::Light, self.light_theme.as_deref());
        let dark = resolve(ThemeMode::Dark, self.dark_theme.as_deref());
        let theme = Theme::global_mut(cx);
        theme.light_theme = light;
        theme.dark_theme = dark;
        Theme::change(self.theme_mode(window), Some(window), cx);
        cx.notify();
    }

    /// Shows theme problems that were not shown before as notifications.
    /// The settings page lists all current ones next to their controls.
    fn report_theme_problems(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current: HashSet<String> = self
            .themes
            .file_errors()
            .iter()
            .chain(self.problems.iter().flatten())
            .cloned()
            .collect();
        for problem in current.difference(&self.reported) {
            window.push_notification(Notification::error(problem.clone()), cx);
        }
        self.reported = current;
    }

    fn select(&self, mode: ThemeMode) -> &ThemeSelect {
        if mode.is_dark() {
            &self.dark_select
        } else {
            &self.light_select
        }
    }

    /// Fills the theme pickers. Listing every theme parses the built-in
    /// ones, so it is done when the settings page opens, not at startup.
    pub fn sync_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let names = self.themes.names(mode);
            let active = Theme::global(cx);
            let current = if mode.is_dark() {
                active.dark_theme.name.clone()
            } else {
                active.light_theme.name.clone()
            };
            let selected = names.iter().position(|name| *name == current);
            self.select(mode).update(cx, |select, cx| {
                select.set_items(names, window, cx);
                select.set_selected_index(selected.map(IndexPath::new), window, cx);
            });
        }
    }

    pub fn set_appearance(
        &mut self,
        appearance: Appearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.appearance == appearance {
            return;
        }
        self.appearance = appearance;
        self.save(
            move |settings| settings.set_appearance(appearance),
            window,
            cx,
        );
        Theme::change(self.theme_mode(window), Some(window), cx);
        cx.notify();
    }

    pub fn set_theme(
        &mut self,
        mode: ThemeMode,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The default needs no setting, which keeps the file minimal.
        let name = (self.themes.default_for(mode).name != name).then(|| name.to_owned());
        if mode.is_dark() {
            self.dark_theme = name.clone();
        } else {
            self.light_theme = name.clone();
        }
        self.save(
            move |settings| match mode {
                ThemeMode::Dark => settings.set_dark_theme(name.as_deref()),
                ThemeMode::Light => settings.set_light_theme(name.as_deref()),
            },
            window,
            cx,
        );
        self.apply_themes(window, cx);
        self.report_theme_problems(window, cx);
    }

    /// Saves `change` in the background, after any earlier save.
    fn save(
        &mut self,
        change: impl FnOnce(&mut Settings) + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous = self.save_task.take();
        let settings_file = self.settings_file.clone();
        self.save_task = Some(cx.spawn_in(window, async move |_, cx| {
            if let Some(previous) = previous {
                previous.await;
            }
            let saved = cx
                .background_spawn(async move {
                    Settings::update(&settings_file, |settings| {
                        change(settings);
                        Ok(())
                    })
                })
                .await;
            if let Err(error) = saved {
                cx.update(|window, cx| {
                    window.push_notification(
                        Notification::error(error.to_string()).title("Couldn’t save the setting"),
                        cx,
                    )
                })
                .ok();
            }
        }));
    }
}

impl Render for AppearanceSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);
        // A problem replaces the row's help text, in the danger color, so it
        // sits next to the control it is about.
        let help = |problem: &Option<String>, help: &'static str| match problem {
            Some(problem) => div().text_color(danger).child(problem.clone()),
            None => div().child(help),
        };

        let current = self.appearance;
        let mode = RadioGroup::horizontal("appearance")
            .children(CHOICES.map(|(_, label)| label))
            .selected_index(CHOICES.iter().position(|(choice, _)| *choice == current))
            .on_change(cx.listener(|this, ix: &usize, window, cx| {
                this.set_appearance(CHOICES[*ix].0, window, cx)
            }));
        let theme_row = |id: &'static str, title: &'static str, mode: ThemeMode, text| {
            setting_row(
                id,
                title,
                help(&self.problems[usize::from(mode.is_dark())], text),
                // The select grows to fill its parent, so the parent sets its width.
                div()
                    .flex_none()
                    .w(rems(14.))
                    .child(Select::new(self.select(mode))),
                cx,
            )
        };
        let light = theme_row(
            "light-theme",
            "Light theme",
            ThemeMode::Light,
            "Used when Backdrop is light.",
        );
        let dark = theme_row(
            "dark-theme",
            "Dark theme",
            ThemeMode::Dark,
            "Used when Backdrop is dark.",
        );

        let dir = self.themes.dir().to_owned();
        let folder_description = v_flex().child(dir.display().to_string()).children(
            self.themes
                .file_errors()
                .iter()
                .map(|error| div().text_color(danger).child(error.clone())),
        );
        let open = Button::new("open-themes-folder")
            .outline()
            .label("Open Folder")
            .on_click(move |_, _, cx| {
                std::fs::create_dir_all(&dir).ok();
                cx.open_with_system(&dir);
            });

        let title = v_flex().gap_1().child("Appearance").child(
            Label::new(
                "Theme files in the themes folder join the lists and reload when they change.",
            )
            .text_sm()
            .text_color(muted),
        );
        GroupBox::new()
            .id("appearance")
            .title(title)
            .gap_4()
            .child(setting_row(
                "appearance-mode",
                "Mode",
                "System follows the desktop and switches with it.",
                mode,
                cx,
            ))
            .child(light)
            .child(dark)
            .child(setting_row(
                "themes-folder",
                "Themes folder",
                folder_description,
                open,
                cx,
            ))
    }
}

#[cfg(test)]
mod tests {
    use backdrop_core::settings::{Appearance, Settings};
    use gpui_kit::component::{ActiveTheme as _, Root, ThemeMode};
    use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext, px, size};

    use super::AppearanceSettings;

    /// A theme set file holding themes of these names and modes.
    fn theme_set(themes: &[(&str, &str)]) -> String {
        let themes: Vec<String> = themes
            .iter()
            .map(|(name, mode)| {
                format!(r#"{{"name": "{name}", "mode": "{mode}", "colors": {{}}}}"#)
            })
            .collect();
        format!(r#"{{"name": "Set", "themes": [{}]}}"#, themes.join(","))
    }

    /// Appearance settings read from `settings` and a themes folder holding
    /// `themes`, in a window.
    fn open(
        settings: Settings,
        themes: &[(&str, &str)],
        cx: &mut TestAppContext,
    ) -> (
        tempfile::TempDir,
        AnyWindowHandle,
        Entity<AppearanceSettings>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let settings_file = dir.path().join("settings.toml");
        let themes_dir = dir.path().join("themes");
        settings.save(&settings_file).unwrap();
        std::fs::create_dir(&themes_dir).unwrap();
        std::fs::write(themes_dir.join("mine.json"), theme_set(themes)).unwrap();

        cx.update(gpui_kit::init);
        let mut view = None;
        let handle = cx.open_window(size(px(800.), px(600.)), |window, cx| {
            let appearance =
                cx.new(|cx| AppearanceSettings::new(settings_file, &themes_dir, window, cx));
            view = Some(appearance.clone());
            Root::new(appearance, window, cx)
        });
        cx.run_until_parked();
        (dir, handle.into(), view.unwrap())
    }

    fn active(cx: &mut TestAppContext) -> (String, String, bool) {
        cx.update(|cx| {
            let theme = cx.theme();
            (
                theme.light_theme.name.to_string(),
                theme.dark_theme.name.to_string(),
                theme.is_dark(),
            )
        })
    }

    #[gpui_kit::test]
    fn configured_themes_and_mode_apply_at_once(cx: &mut TestAppContext) {
        let mut settings = Settings::default();
        settings.set_appearance(Appearance::Dark);
        settings.set_light_theme(Some("Ayu Light"));
        settings.set_dark_theme(Some("Mine"));
        let (_dir, _window, appearance) = open(settings, &[("Mine", "dark")], cx);

        assert_eq!(
            active(cx),
            ("Ayu Light".into(), "Mine".into(), true),
            "a built-in light theme, the user's dark one, and dark mode"
        );
        assert_eq!(
            cx.update(|cx| appearance.read(cx).problems.clone()),
            [None, None]
        );
    }

    #[gpui_kit::test]
    fn unknown_and_wrong_mode_themes_fall_back_to_the_defaults(cx: &mut TestAppContext) {
        let mut settings = Settings::default();
        settings.set_light_theme(Some("Mine"));
        settings.set_dark_theme(Some("Nope"));
        let (_dir, _window, appearance) = open(settings, &[("Mine", "dark")], cx);

        assert_eq!(
            active(cx),
            ("Default Light".into(), "Default Dark".into(), false)
        );
        assert_eq!(
            cx.update(|cx| appearance.read(cx).problems.clone()),
            [
                Some("“Mine” is a dark theme. Using Default Light instead.".into()),
                Some("There is no theme named “Nope”. Using Default Dark instead.".into()),
            ]
        );
    }

    #[gpui_kit::test]
    fn choices_apply_and_are_saved(cx: &mut TestAppContext) {
        let (dir, window, appearance) = open(Settings::default(), &[("Mine", "dark")], cx);

        cx.update_window(window, |_, window, cx| {
            appearance.update(cx, |appearance, cx| {
                appearance.set_appearance(Appearance::Dark, window, cx);
                appearance.set_theme(ThemeMode::Dark, "Mine", window, cx);
                appearance.set_theme(ThemeMode::Light, "Ayu Light", window, cx);
                // Back to the default, which needs no setting.
                appearance.set_theme(ThemeMode::Light, "Default Light", window, cx);
            })
        })
        .unwrap();
        cx.run_until_parked();

        assert_eq!(active(cx), ("Default Light".into(), "Mine".into(), true));
        let saved = Settings::load(&dir.path().join("settings.toml")).unwrap();
        assert_eq!(saved.appearance(), Appearance::Dark);
        assert_eq!(saved.dark_theme(), Some("Mine"));
        assert_eq!(saved.light_theme(), None);
    }

    #[gpui_kit::test]
    fn changed_theme_files_are_reloaded(cx: &mut TestAppContext) {
        let mut settings = Settings::default();
        settings.set_light_theme(Some("Later"));
        let (dir, window, appearance) = open(settings, &[("Mine", "light")], cx);
        assert_eq!(active(cx).0, "Default Light");

        std::fs::write(
            dir.path().join("themes/mine.json"),
            theme_set(&[("Later", "light")]),
        )
        .unwrap();
        // The folder watcher calls this once the files settle.
        cx.update_window(window, |_, window, cx| {
            appearance.update(cx, |appearance, cx| appearance.reload_themes(window, cx))
        })
        .unwrap();

        assert_eq!(active(cx).0, "Later");
        assert_eq!(
            cx.update(|cx| appearance.read(cx).problems.clone()),
            [None, None]
        );
    }
}
