use std::path::PathBuf;
use std::sync::Arc;

use backdrop_core::desktop::WallpaperBackend;
use backdrop_core::library::{Library, LibraryScan, SystemTrash};
use backdrop_core::paths::AppPaths;
use backdrop_core::settings::{LibraryStatus, Settings};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyTitle};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, TitleBar, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::format::count;
use super::image_detail::{BackToGrid, ImageDetail};
use super::library_grid::{LibraryGrid, OpenImage};

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
    _grid_subscription: Option<Subscription>,
    /// The image shown in full, over the grid.
    detail: Option<Entity<ImageDetail>>,
    _detail_subscription: Option<Subscription>,
    /// The desktop shell, once detected; given to each new grid.
    desktop: Option<Arc<dyn WallpaperBackend>>,
    _activation_subscription: Subscription,
    focus_handle: FocusHandle,
}

/// Finds the desktop shell that shows wallpapers, if a supported one runs.
pub type DetectDesktop = fn() -> Option<Box<dyn WallpaperBackend>>;

impl Workspace {
    /// `detect_desktop` finds the desktop shell; tests pass a fake one.
    pub fn new(
        paths: AppPaths,
        detect_desktop: DetectDesktop,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        cx.spawn(async move |this, cx| {
            let desktop = cx.background_spawn(async move { detect_desktop() }).await;
            this.update(cx, |this, cx| {
                this.desktop = desktop.map(Arc::from);
                if let Some(grid) = &this.grid {
                    let desktop = this.desktop.clone();
                    grid.update(cx, |grid, cx| grid.set_desktop(desktop, cx));
                }
            })
            .ok();
        })
        .detach();

        // The shell can't report wallpaper changes, so check whenever the user
        // comes back to Backdrop.
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active()
                && let Some(grid) = &this.grid
            {
                grid.update(cx, |grid, cx| grid.refresh_current_wallpaper(cx));
            }
        });

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
            _grid_subscription: None,
            detail: None,
            _detail_subscription: None,
            desktop: None,
            _activation_subscription: activation,
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
        self.detail = None;
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
                let desktop = self.desktop.clone();
                let grid = cx.new(|cx| {
                    let mut grid = LibraryGrid::new(root, paths, Arc::new(SystemTrash), cx);
                    grid.set_desktop(desktop, cx);
                    grid
                });
                self._grid_subscription = Some(cx.subscribe_in(
                    &grid,
                    window,
                    |this, _, _: &OpenImage, window, cx| this.open_detail(window, cx),
                ));
                let focus_handle = grid.read(cx).focus_handle().clone();
                window.focus(&focus_handle, cx);
                self.grid = Some(grid);
            }
        }
    }

    /// Shows the grid's selected image in full.
    fn open_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(grid) = self.grid.clone() else {
            return;
        };
        let detail = cx.new(|cx| ImageDetail::new(grid, cx));
        self._detail_subscription = Some(cx.subscribe_in(
            &detail,
            window,
            |this, _, _: &BackToGrid, window, cx| this.close_detail(window, cx),
        ));
        let focus_handle = detail.read(cx).focus_handle().clone();
        window.focus(&focus_handle, cx);
        self.detail = Some(detail);
        cx.notify();
    }

    /// Returns to the grid, scrolled to the image that was shown last.
    fn close_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.detail = None;
        self._detail_subscription = None;
        if let Some(grid) = &self.grid {
            grid.update(cx, |grid, cx| {
                if let Some(ix) = grid.selected_ix() {
                    grid.select(ix, cx);
                }
            });
            let focus_handle = grid.read(cx).focus_handle().clone();
            window.focus(&focus_handle, cx);
        }
        cx.notify();
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
                if let Some(detail) = &self.detail {
                    return detail.clone().into_any_element();
                }
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
    /// A way back to the grid and the shown image's position, for the title
    /// bar.
    fn render_detail_summary(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.detail.as_ref()?;
        let grid = self.grid.as_ref()?.read(cx);
        let position = grid.selected_ix().map(|ix| {
            format!(
                "{} of {}",
                ix + 1,
                count(grid.images().len(), "image", "images")
            )
        });
        let folder: SharedString = grid
            .root()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into();
        Some(
            h_flex()
                .gap_2()
                .child(
                    Button::new("back-to-grid")
                        .ghost()
                        .small()
                        .icon(IconName::ChevronLeft)
                        .label(folder)
                        .tooltip("Back to all images (Esc)")
                        .on_click(cx.listener(|this, _, window, cx| this.close_detail(window, cx))),
                )
                .when_some(position, |row, position| {
                    row.child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(position),
                    )
                })
                .into_any_element(),
        )
    }

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
                let total = self
                    .grid
                    .as_ref()
                    .and_then(|grid| grid.read(cx).total())
                    .unwrap_or(scan.index().total());
                let images = count(total, "image", "images");
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
                        self.render_detail_summary(cx)
                            .or_else(|| {
                                self.render_library_summary(cx)
                                    .map(IntoElement::into_any_element)
                            })
                            .unwrap_or_else(|| "Backdrop".into_any_element()),
                    )
                    .when(self.grid.is_some() && self.detail.is_none(), |bar| {
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
    use backdrop_core::library::Library;
    use backdrop_core::paths::AppPaths;
    use backdrop_core::settings::Settings;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, Entity, SharedString, TestAppContext, px, size};
    use image::{Rgb, RgbImage};

    use super::Workspace;

    /// A workspace whose library folder holds `count` small images.
    fn open_workspace(
        count: u8,
        cx: &mut TestAppContext,
    ) -> (
        tempfile::TempDir,
        AppPaths,
        gpui_kit::AnyWindowHandle,
        Entity<Workspace>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir(&root).unwrap();
        for ix in 0..count {
            RgbImage::from_pixel(64, 40, Rgb([ix * 30, 80, 120]))
                .save(root.join(format!("{ix}.png")))
                .unwrap();
        }
        let paths = AppPaths::new(
            dir.path().join("config"),
            dir.path().join("data"),
            dir.path().join("cache"),
        );
        Settings::update(&paths.settings_file(), |settings| {
            settings.set_library_dir(&root)
        })
        .unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            super::super::bind_keys(cx);
        });
        let mut workspace = None;
        let window_paths = paths.clone();
        let handle = cx.open_window(size(px(800.), px(600.)), |window, cx| {
            let view = cx.new(|cx| Workspace::new(window_paths, || None, window, cx));
            workspace = Some(view.clone());
            Root::new(view, window, cx)
        });
        cx.run_until_parked();
        (dir, paths, handle.into(), workspace.unwrap())
    }

    #[gpui_kit::test]
    fn enter_opens_the_selected_image_and_escape_returns_to_it(cx: &mut TestAppContext) {
        let (_dir, paths, window, workspace) = open_workspace(4, cx);
        let grid = cx.update(|cx| workspace.read(cx).grid.clone().expect("grid after scan"));
        let tile = |ix: usize, cx: &mut TestAppContext| -> SharedString {
            cx.update(|cx| {
                grid.read(cx).images()[ix]
                    .path()
                    .to_string_lossy()
                    .into_owned()
                    .into()
            })
        };
        let first = tile(0, cx);
        let selected = |cx: &mut TestAppContext| cx.update(|cx| grid.read(cx).selected_ix());
        let detail_open =
            |cx: &mut TestAppContext| cx.update(|cx| workspace.read(cx).detail.is_some());

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.click(first.clone(), cx);
            window.press("enter", cx);
        })
        .unwrap();
        assert!(detail_open(cx));

        cx.update_window(window, |_, window, cx| {
            window.press("right", cx);
            window.press("right", cx);
            window.press("f", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(selected(cx), Some(2));
        let favorite = cx.update(|cx| grid.read(cx).images()[2].path().to_path_buf());
        let root = cx.update(|cx| grid.read(cx).root().to_path_buf());
        let favorites = Library::open(&root, &paths.database_file())
            .unwrap()
            .favorites()
            .unwrap();
        assert!(favorites.contains(&favorite));

        cx.update_window(window, |_, window, cx| window.press("escape", cx))
            .unwrap();
        assert!(!detail_open(cx));
        assert_eq!(selected(cx), Some(2), "the grid keeps the last image shown");

        // Focus is back in the grid, so arrow keys move its selection again.
        cx.update_window(window, |_, window, cx| window.press("left", cx))
            .unwrap();
        assert_eq!(selected(cx), Some(1));
    }
}
