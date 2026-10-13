use std::path::PathBuf;
use std::sync::Arc;

use backdrop_core::desktop::WallpaperBackend;
use backdrop_core::library::{Library, LibraryScan, SystemTrash};
use backdrop_core::paths::AppPaths;
use backdrop_core::providers::Provider;
use backdrop_core::remote_images::RemotePreviews;
use backdrop_core::settings::{LibraryStatus, Settings};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyTitle};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, TitleBar, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::image_detail::{BackToGrid, ImageDetail};
use super::library_grid::{LibraryGrid, OpenImage};
use super::wallhaven_browser::{LibraryChanged, LibraryLocation, OpenWallpaper, WallhavenBrowser};
use super::wallhaven_detail::{BackToResults, WallhavenDetail};
use crate::format::count;

pub const KEY_CONTEXT: &str = "Workspace";

/// Full Wallhaven images kept in the cache for the detail view.
const PREVIEWS_KEPT: usize = 30;

actions!(backdrop, [ChooseLibraryFolder, ShowLibrary, ShowWallhaven]);

/// What the window shows below its title bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Library,
    Wallhaven,
}

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
    page: Page,
    /// Where Wallhaven wallpapers come from; a fake one in tests.
    provider: Arc<dyn Provider>,
    /// Created the first time the Wallhaven page is shown, so nothing is
    /// requested from Wallhaven until then.
    wallhaven: Option<Entity<WallhavenBrowser>>,
    _wallhaven_subscriptions: Vec<Subscription>,
    /// The Wallhaven result shown in full, over the results.
    wallhaven_detail: Option<Entity<WallhavenDetail>>,
    _wallhaven_detail_subscription: Option<Subscription>,
    _activation_subscription: Subscription,
    focus_handle: FocusHandle,
}

/// Finds the desktop shell that shows wallpapers, if a supported one runs.
pub type DetectDesktop = fn() -> Option<Box<dyn WallpaperBackend>>;

impl Workspace {
    /// `detect_desktop` finds the desktop shell and `provider` serves Wallhaven
    /// wallpapers; tests pass fakes.
    pub fn new(
        paths: AppPaths,
        detect_desktop: DetectDesktop,
        provider: Arc<dyn Provider>,
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
                .background_spawn(async move { Settings::load(&settings_file) })
                .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(settings) => {
                    let key = settings.wallhaven_api_key().map(str::to_owned);
                    this.provider.set_api_key(key);
                    this.set_library_status(settings.library_status(), window, cx)
                }
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
            page: Page::Library,
            provider,
            wallhaven: None,
            _wallhaven_subscriptions: Vec::new(),
            wallhaven_detail: None,
            _wallhaven_detail_subscription: None,
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
        if let Some(browser) = &self.wallhaven {
            let location = self.library_location();
            browser.update(cx, |browser, cx| browser.set_library(location, cx));
        }
        cx.notify();
    }

    /// Where Wallhaven downloads go, when a library folder is available.
    fn library_location(&self) -> Option<LibraryLocation> {
        match &self.library {
            LibraryState::Loaded(LibraryStatus::Available(root)) => Some(LibraryLocation {
                root: root.clone(),
                database: self.paths.database_file(),
            }),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn page(&self) -> Page {
        self.page
    }

    #[cfg(test)]
    pub fn wallhaven(&self) -> Option<&Entity<WallhavenBrowser>> {
        self.wallhaven.as_ref()
    }

    /// Shows `page` and moves focus into it. The Wallhaven page starts with its
    /// search field focused the first time.
    pub fn show_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.page = page;
        let focus_handle = match page {
            Page::Library => self
                .detail
                .as_ref()
                .map(|detail| detail.read(cx).focus_handle().clone())
                .or_else(|| {
                    self.grid
                        .as_ref()
                        .map(|grid| grid.read(cx).focus_handle().clone())
                })
                .unwrap_or_else(|| self.focus_handle.clone()),
            Page::Wallhaven => match (&self.wallhaven_detail, &self.wallhaven) {
                (Some(detail), _) => detail.read(cx).focus_handle().clone(),
                (None, Some(browser)) => browser.read(cx).focus_handle().clone(),
                (None, None) => {
                    let browser = self.open_wallhaven(window, cx);
                    browser.update(cx, |browser, cx| browser.focus_search(window, cx));
                    cx.notify();
                    return;
                }
            },
        };
        window.focus(&focus_handle, cx);
        cx.notify();
    }

    fn open_wallhaven(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<WallhavenBrowser> {
        let provider = self.provider.clone();
        let location = self.library_location();
        let thumbnails_dir = self.paths.remote_thumbnails_dir();
        let settings_file = self.paths.settings_file();
        let browser = cx.new(|cx| {
            WallhavenBrowser::new(
                provider,
                location,
                thumbnails_dir,
                settings_file,
                window,
                cx,
            )
        });
        self._wallhaven_subscriptions = vec![
            cx.subscribe_in(
                &browser,
                window,
                |this, _, _: &LibraryChanged, window, cx| this.library_changed(window, cx),
            ),
            cx.subscribe_in(
                &browser,
                window,
                |this, _, _: &OpenWallpaper, window, cx| this.open_wallhaven_detail(window, cx),
            ),
        ];
        self.wallhaven = Some(browser.clone());
        browser
    }

    /// Shows the browser's selected wallpaper in full.
    fn open_wallhaven_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(browser) = self.wallhaven.clone() else {
            return;
        };
        let provider = self.provider.clone();
        let previews = RemotePreviews::new(self.paths.remote_previews_dir(), PREVIEWS_KEPT);
        let detail = cx.new(|cx| WallhavenDetail::new(browser, provider, previews, cx));
        self._wallhaven_detail_subscription = Some(cx.subscribe_in(
            &detail,
            window,
            |this, _, _: &BackToResults, window, cx| this.close_wallhaven_detail(window, cx),
        ));
        let focus_handle = detail.read(cx).focus_handle().clone();
        window.focus(&focus_handle, cx);
        self.wallhaven_detail = Some(detail);
        cx.notify();
    }

    /// Returns to the results, scrolled to the wallpaper shown last.
    fn close_wallhaven_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.wallhaven_detail = None;
        self._wallhaven_detail_subscription = None;
        if let Some(browser) = &self.wallhaven {
            browser.update(cx, |browser, cx| {
                if let Some(ix) = browser.selected_ix() {
                    browser.select(ix, cx);
                }
            });
            let focus_handle = browser.read(cx).focus_handle().clone();
            window.focus(&focus_handle, cx);
        }
        cx.notify();
    }

    /// A way back to the results and the shown wallpaper's position, for the
    /// title bar.
    fn render_wallhaven_summary(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.wallhaven_detail.as_ref()?;
        let browser = self.wallhaven.as_ref()?.read(cx);
        let position = browser.selected_ix().map(|ix| {
            format!(
                "{} of {}",
                ix + 1,
                count(browser.total() as usize, "wallpaper", "wallpapers")
            )
        });
        Some(
            h_flex()
                .gap_2()
                .child(
                    Button::new("back-to-results")
                        .ghost()
                        .small()
                        .icon(IconName::ChevronLeft)
                        .label("Results")
                        .tooltip("Back to the results (Esc)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.close_wallhaven_detail(window, cx)
                        })),
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

    /// A download added an image: show it in the library. The download has
    /// already indexed it, so the grid only needs to reread the index; a
    /// library that was empty has no grid yet, so it is scanned instead.
    fn library_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let LibraryState::Loaded(LibraryStatus::Available(root)) = &self.library else {
            return;
        };
        match &self.grid {
            Some(grid) => grid.update(cx, |grid, cx| grid.reload(cx)),
            None => self.scan_library(root.clone(), window, cx),
        }
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
                if let Some(browser) = &this.wallhaven {
                    browser.update(cx, |browser, cx| browser.refresh_library(cx));
                }
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
        let detail = cx.new(|cx| {
            ImageDetail::new(grid, self.provider.clone(), self.paths.database_file(), cx)
        });
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

    /// The switch between the library and Wallhaven.
    fn render_pages(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = match self.page {
            Page::Library => 0,
            Page::Wallhaven => 1,
        };
        TabBar::new("pages")
            .segmented()
            .small()
            .selected_index(selected)
            .child(Tab::new().label("Library"))
            .child(Tab::new().label("Wallhaven"))
            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                let page = if *ix == 0 {
                    Page::Library
                } else {
                    Page::Wallhaven
                };
                this.show_page(page, window, cx)
            }))
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
        let library_page = self.page == Page::Library;
        v_flex()
            .track_focus(&self.focus_handle)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &ChooseLibraryFolder, window, cx| {
                this.choose_library_folder(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowLibrary, window, cx| {
                this.show_page(Page::Library, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowWallhaven, window, cx| {
                this.show_page(Page::Wallhaven, window, cx)
            }))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                TitleBar::new()
                    .child(h_flex().gap_3().child(self.render_pages(cx)).map(|row| {
                        match library_page {
                            true => row.children(self.render_detail_summary(cx).or_else(|| {
                                self.render_library_summary(cx)
                                    .map(IntoElement::into_any_element)
                            })),
                            false => row.children(self.render_wallhaven_summary(cx)),
                        }
                    }))
                    .when(
                        library_page && self.grid.is_some() && self.detail.is_none(),
                        |bar| {
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
                        },
                    ),
            )
            .child(v_flex().flex_1().min_h_0().map(|content| {
                match (self.page, &self.wallhaven, &self.wallhaven_detail) {
                    (Page::Wallhaven, _, Some(detail)) => content.child(detail.clone()),
                    (Page::Wallhaven, Some(browser), None) => content.child(browser.clone()),
                    _ => content.child(self.render_library(cx)),
                }
            }))
    }
}

#[cfg(test)]
mod tests {
    use backdrop_core::library::Library;
    use backdrop_core::paths::AppPaths;
    use backdrop_core::settings::LibraryStatus;
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
        let images: Vec<_> = (0..count).map(|ix| (format!("{ix}.png"), 64, 40)).collect();
        open_workspace_with(&images, Vec::new(), None, cx)
    }

    /// A workspace whose library folder holds images of these names and sizes,
    /// with Wallhaven results served from `pages`.
    fn open_workspace_with(
        images: &[(String, u32, u32)],
        pages: Vec<Vec<&'static str>>,
        provider: Option<std::sync::Arc<crate::test_provider::FakeProvider>>,
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
        for (ix, (name, width, height)) in images.iter().enumerate() {
            RgbImage::from_pixel(*width, *height, Rgb([ix as u8 * 30, 80, 120]))
                .save(root.join(name))
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
            let view = cx.new(|cx| {
                Workspace::new(
                    window_paths,
                    || None,
                    provider.clone().unwrap_or_else(|| {
                        std::sync::Arc::new(crate::test_provider::FakeProvider::new(pages.clone()))
                    }),
                    window,
                    cx,
                )
            });
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
    #[gpui_kit::test]
    fn ctrl_2_shows_wallhaven_and_ctrl_1_returns_to_the_library(cx: &mut TestAppContext) {
        let (_dir, _paths, window, workspace) = open_workspace(2, cx);
        assert!(cx.update(|cx| workspace.read(cx).wallhaven().is_none()));

        cx.update_window(window, |_, window, cx| window.press("ctrl-2", cx))
            .unwrap();
        cx.run_until_parked();
        let page = |cx: &mut TestAppContext| cx.update(|cx| workspace.read(cx).page());
        assert_eq!(page(cx), super::Page::Wallhaven);
        assert!(cx.update(|cx| workspace.read(cx).wallhaven().is_some()));

        // The search field has focus, and ctrl-1 still switches back.
        cx.update_window(window, |_, window, cx| window.press("ctrl-1", cx))
            .unwrap();
        assert_eq!(page(cx), super::Page::Library);
        // Focus is back in the grid, so arrow keys select images.
        let grid = cx.update(|cx| workspace.read(cx).grid.clone().unwrap());
        cx.update_window(window, |_, window, cx| window.press("right", cx))
            .unwrap();
        assert_eq!(cx.update(|cx| grid.read(cx).selected_ix()), Some(0));
    }
    #[gpui_kit::test]
    fn get_details_links_a_recognized_image_and_shows_its_tags(cx: &mut TestAppContext) {
        // Named like a download from the fake provider, at the size its
        // wallpapers report.
        let images = [("fake-aaa111.png".to_owned(), 20, 10)];
        let (_dir, paths, window, workspace) = open_workspace_with(&images, Vec::new(), None, cx);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.click("fake-aaa111.png", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let detail = cx.update(|cx| workspace.read(cx).detail.clone().expect("detail open"));
        assert!(cx.update(|cx| detail.read(cx).tags(cx).is_empty()));

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("get-details", cx);
        })
        .unwrap();
        cx.run_until_parked();

        assert_eq!(
            cx.update(|cx| detail.read(cx).tags(cx)),
            ["mountains", "lake"]
        );
        let root = cx.update(|cx| {
            workspace
                .read(cx)
                .grid
                .clone()
                .unwrap()
                .read(cx)
                .root()
                .to_path_buf()
        });
        let metadata = Library::open(&root, &paths.database_file())
            .unwrap()
            .metadata()
            .unwrap();
        let record = metadata
            .get(std::path::Path::new("fake-aaa111.png"))
            .unwrap();
        assert_eq!(record.source(), Some("fake/aaa111"));
    }
    #[gpui_kit::test]
    fn enter_shows_a_wallhaven_result_with_its_tags_and_full_image(cx: &mut TestAppContext) {
        let pages = vec![vec!["aaa111", "bbb222", "ccc333"]];
        let (_dir, paths, window, workspace) = open_workspace_with(&[], pages, None, cx);
        cx.update_window(window, |_, window, cx| window.press("ctrl-2", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.click("wallpaper-aaa111", cx);
            window.press("enter", cx);
        })
        .unwrap();
        let detail = cx.update(|cx| {
            workspace
                .read(cx)
                .wallhaven_detail
                .clone()
                .expect("detail open")
        });
        let browser = cx.update(|cx| workspace.read(cx).wallhaven.clone().unwrap());
        let settle = |cx: &mut TestAppContext| {
            cx.executor()
                .advance_clock(std::time::Duration::from_secs(1));
            cx.run_until_parked();
        };
        settle(cx);

        assert_eq!(
            cx.update(|cx| detail.read(cx).tags(cx)),
            ["mountains", "lake"]
        );
        let preview = cx
            .update(|cx| detail.read(cx).preview_file())
            .expect("full image");
        assert!(preview.starts_with(paths.remote_previews_dir()));

        cx.update_window(window, |_, window, cx| window.press("right", cx))
            .unwrap();
        settle(cx);
        assert_eq!(cx.update(|cx| browser.read(cx).selected_ix()), Some(1));
        assert_eq!(
            cx.update(|cx| detail.read(cx).tags(cx)),
            ["mountains", "lake"]
        );

        cx.update_window(window, |_, window, cx| window.press("escape", cx))
            .unwrap();
        assert!(cx.update(|cx| workspace.read(cx).wallhaven_detail.is_none()));
        assert_eq!(cx.update(|cx| browser.read(cx).selected_ix()), Some(1));
        // Focus is back in the results, so arrow keys move their selection.
        cx.update_window(window, |_, window, cx| window.press("right", cx))
            .unwrap();
        assert_eq!(cx.update(|cx| browser.read(cx).selected_ix()), Some(2));
    }
    #[gpui_kit::test]
    fn stepping_quickly_only_loads_the_wallpaper_it_stops_on(cx: &mut TestAppContext) {
        let provider = std::sync::Arc::new(crate::test_provider::FakeProvider::new(vec![vec![
            "aaa111", "bbb222", "ccc333",
        ]]));
        let (_dir, paths, window, _workspace) =
            open_workspace_with(&[], Vec::new(), Some(provider.clone()), cx);
        cx.update_window(window, |_, window, cx| window.press("ctrl-2", cx))
            .unwrap();
        cx.run_until_parked();

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.click("wallpaper-aaa111", cx);
            window.press("enter", cx);
            window.press("right", cx);
            window.press("right", cx);
        })
        .unwrap();
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();

        assert_eq!(provider.detail_requests(), ["ccc333"]);
        let previews = std::fs::read_dir(paths.remote_previews_dir().join("fake"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(previews, ["ccc333.preview"]);
    }
    #[gpui_kit::test]
    fn downloading_from_the_detail_view_reuses_its_full_image(cx: &mut TestAppContext) {
        let provider = std::sync::Arc::new(crate::test_provider::FakeProvider::new(vec![vec![
            "aaa111",
        ]]));
        let (_dir, _paths, window, workspace) =
            open_workspace_with(&[], Vec::new(), Some(provider.clone()), cx);
        cx.update_window(window, |_, window, cx| window.press("ctrl-2", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.click("wallpaper-aaa111", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(provider.downloads(), 1, "the full image for the preview");

        cx.update_window(window, |_, window, cx| window.press("d", cx))
            .unwrap();
        cx.run_until_parked();

        let root = cx.update(|cx| match &workspace.read(cx).library {
            super::LibraryState::Loaded(LibraryStatus::Available(root)) => root.clone(),
            _ => panic!("no library"),
        });
        assert!(root.join("fake-aaa111.png").is_file());
        assert_eq!(provider.downloads(), 1, "no second download");
    }
}
