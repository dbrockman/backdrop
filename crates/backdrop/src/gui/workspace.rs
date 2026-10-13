use std::path::PathBuf;

use backdrop_core::library::{Library, LibraryScan};
use backdrop_core::paths::AppPaths;
use backdrop_core::settings::{LibraryStatus, Settings};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyTitle};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, TitleBar, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::library_grid::LibraryGrid;

pub const KEY_CONTEXT: &str = "Workspace";

actions!(backdrop, [ChooseLibraryFolder]);

/// The library as far as the window knows it.
enum LibraryState {
    Loading,
    /// The settings file could not be read. Nothing is saved in this state, so
    /// the file is never overwritten with defaults.
    Unreadable(SharedString),
    Loaded(LibraryStatus),
}

/// The latest scan of an available library folder.
enum Scan {
    Running,
    Finished(Box<LibraryScan>),
    Failed(SharedString),
}

/// The main window's content: title bar above the library.
pub struct Workspace {
    paths: AppPaths,
    library: LibraryState,
    /// A folder prompt or settings save is in progress.
    choosing: bool,
    scan: Scan,
    /// Dropping it discards the result of a scan that is no longer wanted.
    scan_task: Option<Task<()>>,
    /// Shown once the first scan of the library has finished.
    grid: Option<Entity<LibraryGrid>>,
    focus_handle: FocusHandle,
}

impl Workspace {
    pub fn new(paths: AppPaths, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let settings_file = paths.settings_file();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    Settings::load(&settings_file).map(|settings| settings.library_status())
                })
                .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(status) => this.set_library_status(status, window, cx),
                Err(error) => {
                    this.library = LibraryState::Unreadable(error.to_string().into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();

        Self {
            paths,
            library: LibraryState::Loading,
            choosing: false,
            scan: Scan::Running,
            scan_task: None,
            grid: None,
            focus_handle,
        }
    }

    fn set_library_status(
        &mut self,
        status: LibraryStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.grid = None;
        if let LibraryStatus::Available(root) = &status {
            self.scan_library(root.clone(), window, cx);
        }
        self.library = LibraryState::Loaded(status);
        cx.notify();
    }

    /// Brings the index and metadata up to date with the library in the
    /// background.
    fn scan_library(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.scan = Scan::Running;
        let database = self.paths.database_file();
        self.scan_task = Some(cx.spawn_in(window, async move |this, cx| {
            let scan_root = root.clone();
            let result = cx
                .background_spawn(async move { Library::open(&scan_root, &database)?.scan() })
                .await;
            this.update_in(cx, |this, window, cx| {
                if let Ok(scan) = &result {
                    match scan.metadata() {
                        Ok(metadata) if !metadata.unconfirmed_removals().is_empty() => {
                            let paths = metadata.unconfirmed_removals().to_vec();
                            this.confirm_removals(root.clone(), paths, window, cx);
                        }
                        Ok(_) => {}
                        Err(error) => window.push_notification(
                            Notification::error(error.to_string())
                                .title("Couldn't update the library metadata"),
                            cx,
                        ),
                    }
                }
                if let Ok(scan) = &result
                    && scan.index().total() > 0
                {
                    this.show_grid(root.clone(), window, cx);
                }
                this.scan = match result {
                    Ok(scan) => Scan::Finished(Box::new(scan)),
                    Err(error) => Scan::Failed(error.to_string().into()),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn show_grid(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        match &self.grid {
            Some(grid) => grid.update(cx, |grid, cx| grid.reload(cx)),
            None => {
                let paths = self.paths.clone();
                let grid = cx.new(|cx| LibraryGrid::new(root, paths, cx));
                let focus_handle = grid.read(cx).focus_handle().clone();
                window.focus(&focus_handle, cx);
                self.grid = Some(grid);
            }
        }
    }

    /// Asks before removing the metadata of many missing images at once. If
    /// the user keeps it, the next scan asks again.
    fn confirm_removals(
        &mut self,
        root: PathBuf,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = cx.weak_entity();
        let title: SharedString = format!(
            "Remove metadata for {}?",
            count(paths.len(), "missing image", "missing images")
        )
        .into();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let workspace = workspace.clone();
            let root = root.clone();
            let paths = paths.clone();
            alert
                .title(title.clone())
                .description(
                    "These images are no longer in the library folder. Their favorites, tags, \
                     and sources will be removed.",
                )
                .confirm()
                .ok_text("Remove")
                .ok_variant(ButtonVariant::Danger)
                .cancel_text("Keep")
                .on_ok(move |_, window, cx| {
                    workspace
                        .update(cx, |this, cx| {
                            this.remove_missing(root.clone(), paths.clone(), window, cx)
                        })
                        .ok();
                    true
                })
        });
    }

    fn remove_missing(
        &mut self,
        root: PathBuf,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let database = self.paths.database_file();
        cx.spawn_in(window, async move |_, cx| {
            let result = cx
                .background_spawn(
                    async move { Library::open(&root, &database)?.remove_missing(&paths) },
                )
                .await;
            if let Err(error) = result {
                cx.update(|window, cx| {
                    window.push_notification(
                        Notification::error(error.to_string())
                            .title("Couldn't remove the metadata"),
                        cx,
                    )
                })
                .ok();
            }
        })
        .detach();
    }

    /// Asks for a folder and saves it as the library folder.
    fn choose_library_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.choosing || !matches!(self.library, LibraryState::Loaded(_)) {
            return;
        }
        self.choosing = true;
        cx.notify();

        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        let settings_file = self.paths.settings_file();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = match picked.await {
                Ok(Ok(Some(mut dirs))) => match dirs.pop() {
                    Some(dir) => Some(
                        cx.background_spawn(async move {
                            Settings::update(&settings_file, |settings| {
                                settings.set_library_dir(&dir)
                            })
                            .map(|(settings, _)| settings.library_status())
                        })
                        .await
                        .map_err(|error| ("Couldn't change the library folder", error.to_string())),
                    ),
                    None => None,
                },
                // Cancelled, or the prompt went away with the window.
                Ok(Ok(None)) | Err(_) => None,
                Ok(Err(error)) => Some(Err(("Couldn't open the folder picker", error.to_string()))),
            };
            this.update_in(cx, |this, window, cx| {
                this.choosing = false;
                match outcome {
                    Some(Ok(status)) => this.set_library_status(status, window, cx),
                    Some(Err((title, message))) => {
                        window.push_notification(Notification::error(message).title(title), cx)
                    }
                    None => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_library(&self, cx: &mut Context<Self>) -> AnyElement {
        let status = match &self.library {
            LibraryState::Loading => return div().into_any_element(),
            LibraryState::Unreadable(message) => {
                return empty_state("Couldn't read settings", message.clone(), None)
                    .into_any_element();
            }
            LibraryState::Loaded(status) => status,
        };

        let choose = Button::new("choose-library-folder")
            .loading(self.choosing)
            .on_click(cx.listener(|this, _, window, cx| this.choose_library_folder(window, cx)));

        match status {
            LibraryStatus::NotChosen => empty_state(
                "Choose a wallpaper folder",
                "Backdrop shows the images in it and its subfolders. Files stay where they are.",
                Some(choose.primary().label("Choose Folder…")),
            ),
            LibraryStatus::Missing(dir) => empty_state(
                "Library folder not found",
                dir.display().to_string(),
                Some(choose.primary().label("Choose Folder…")),
            ),
            LibraryStatus::Available(_) => {
                if let Some(grid) = &self.grid {
                    return grid.clone().into_any_element();
                }
                match &self.scan {
                    Scan::Running => empty_state("Scanning…", "", None),
                    Scan::Failed(message) => empty_state(
                        "Couldn't scan the library",
                        message.clone(),
                        Some(choose.outline().label("Change Folder…")),
                    ),
                    Scan::Finished(_) => empty_state(
                        "No images in this folder",
                        "Add JPEG, PNG, or WebP images to it or its subfolders.",
                        Some(choose.outline().label("Change Folder…")),
                    ),
                }
            }
        }
        .m_4()
        .into_any_element()
    }

    /// The library folder's name and image count, for the title bar.
    fn render_library_summary(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let LibraryState::Loaded(LibraryStatus::Available(dir)) = &self.library else {
            return None;
        };
        let name = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string());
        let details = match &self.scan {
            Scan::Running => Some("Scanning…".to_owned()),
            Scan::Failed(_) => None,
            Scan::Finished(scan) => {
                let images = count(scan.index().total(), "image", "images");
                Some(match scan.index().issues().len() {
                    0 => images,
                    skipped => format!("{images} · {} skipped", count(skipped, "file", "files")),
                })
            }
        };
        Some(
            h_flex()
                .gap_2()
                .child(name)
                .when_some(details, |row, details| {
                    row.child(div().text_color(cx.theme().muted_foreground).child(details))
                }),
        )
    }
}

/// Formats `n` with thousands separators and the matching noun, e.g. "1,234 images".
fn count(n: usize, one: &str, many: &str) -> String {
    let digits = n.to_string();
    let mut grouped = String::new();
    for (ix, digit) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    format!("{grouped} {}", if n == 1 { one } else { many })
}

fn empty_state(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    action: Option<Button>,
) -> Empty {
    let empty = Empty::new().header(
        EmptyHeader::new()
            .title(EmptyTitle::new().child(title.into()))
            .description(EmptyDescription::new().child(description.into())),
    );
    match action {
        Some(action) => empty.content(EmptyContent::new().child(action)),
        None => empty,
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .track_focus(&self.focus_handle)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &ChooseLibraryFolder, window, cx| {
                this.choose_library_folder(window, cx)
            }))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                TitleBar::new()
                    .child(
                        self.render_library_summary(cx)
                            .map(IntoElement::into_any_element)
                            .unwrap_or_else(|| "Backdrop".into_any_element()),
                    )
                    .when(self.grid.is_some(), |bar| {
                        bar.child(
                            Button::new("change-library-folder")
                                .ghost()
                                .small()
                                .mr_2()
                                .label("Change Folder…")
                                .loading(self.choosing)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.choose_library_folder(window, cx)
                                })),
                        )
                    }),
            )
            .child(v_flex().flex_1().min_h_0().child(self.render_library(cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::count;

    #[test]
    fn counts_are_grouped_and_pluralized() {
        assert_eq!(count(0, "image", "images"), "0 images");
        assert_eq!(count(1, "image", "images"), "1 image");
        assert_eq!(count(999, "image", "images"), "999 images");
        assert_eq!(count(1234, "image", "images"), "1,234 images");
        assert_eq!(count(1234567, "image", "images"), "1,234,567 images");
    }
}
