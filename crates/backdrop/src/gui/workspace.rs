use std::path::PathBuf;

use backdrop_core::index::{Index, ScanSummary};
use backdrop_core::paths::AppPaths;
use backdrop_core::settings::{LibraryStatus, Settings};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyTitle};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, TitleBar, WindowExt as _, v_flex};
use gpui_kit::*;

pub const KEY_CONTEXT: &str = "Workspace";

actions!(backdrop, [ChooseLibraryFolder]);

/// The library as far as the window knows it.
enum Library {
    Loading,
    /// The settings file could not be read. Nothing is saved in this state, so
    /// the file is never overwritten with defaults.
    Unreadable(SharedString),
    Loaded(LibraryStatus),
}

/// The latest scan of an available library folder.
enum Scan {
    Running,
    Finished(ScanSummary),
    Failed(SharedString),
}

/// The main window's content: title bar above the library.
pub struct Workspace {
    paths: AppPaths,
    library: Library,
    /// A folder prompt or settings save is in progress.
    choosing: bool,
    scan: Scan,
    /// Dropping it discards the result of a scan that is no longer wanted.
    scan_task: Option<Task<()>>,
    focus_handle: FocusHandle,
}

impl Workspace {
    pub fn new(paths: AppPaths, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let settings_file = paths.settings_file();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    Settings::load(&settings_file).map(|settings| settings.library_status())
                })
                .await;
            this.update(cx, |this, cx| match result {
                Ok(status) => this.set_library_status(status, cx),
                Err(error) => {
                    this.library = Library::Unreadable(error.to_string().into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();

        Self {
            paths,
            library: Library::Loading,
            choosing: false,
            scan: Scan::Running,
            scan_task: None,
            focus_handle,
        }
    }

    fn set_library_status(&mut self, status: LibraryStatus, cx: &mut Context<Self>) {
        if let LibraryStatus::Available(root) = &status {
            self.scan_library(root.clone(), cx);
        }
        self.library = Library::Loaded(status);
        cx.notify();
    }

    /// Brings the index up to date with the library in the background.
    fn scan_library(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.scan = Scan::Running;
        let database = self.paths.database_file();
        self.scan_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { Index::open(&database)?.scan(&root) })
                .await;
            this.update(cx, |this, cx| {
                this.scan = match result {
                    Ok(summary) => Scan::Finished(summary),
                    Err(error) => Scan::Failed(error.to_string().into()),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    /// Asks for a folder and saves it as the library folder.
    fn choose_library_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.choosing || !matches!(self.library, Library::Loaded(_)) {
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
                    Some(Ok(status)) => this.set_library_status(status, cx),
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
            Library::Loading => return div().into_any_element(),
            Library::Unreadable(message) => {
                return empty_state("Couldn't read settings", message.clone(), None)
                    .into_any_element();
            }
            Library::Loaded(status) => status,
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
            // Stands in for the image grid.
            LibraryStatus::Available(dir) => empty_state(
                dir.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| dir.display().to_string()),
                self.scan_description(),
                Some(choose.outline().label("Change Folder…")),
            ),
        }
        .into_any_element()
    }

    fn scan_description(&self) -> SharedString {
        match &self.scan {
            Scan::Running => "Scanning…".into(),
            Scan::Failed(message) => message.clone(),
            Scan::Finished(summary) => {
                let images = count(summary.total(), "image", "images");
                match summary.issues().len() {
                    0 => images.into(),
                    skipped => {
                        format!("{images} · {} skipped", count(skipped, "file", "files")).into()
                    }
                }
            }
        }
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
            .child(TitleBar::new().child("Backdrop"))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_4()
                    .child(self.render_library(cx)),
            )
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
