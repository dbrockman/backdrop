//! Browsing Wallhaven: a search toolbar, a grid of results, and downloading
//! the selected wallpaper into the library.
//!
//! Only safe-for-work wallpapers are shown. Results arrive a page at a time;
//! the next page loads from the button after the last row, or when the keyboard
//! selection reaches the last row. Thumbnails are downloaded in the background
//! into the XDG cache, in display order, and a tile shows a placeholder until
//! its thumbnail is ready. Wallpapers the library already has are marked, and
//! downloads go to the folder the library's download rules choose.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use backdrop_core::image::Orientation;
use backdrop_core::library::{Downloaded, Library};
use backdrop_core::providers::{Provider, RemoteWallpaper, SearchQuery, SearchSort};
use backdrop_core::remote_thumbnails::RemoteThumbnails;
use gpui_kit::component::button::{
    Button, ButtonVariants as _, Toggle, ToggleGroup, ToggleVariants as _,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::link::Link;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::image_cache::LruImageCache;
use super::library_grid::{SelectDown, SelectFirst, SelectLast, SelectLeft, SelectRight, SelectUp};
use super::tiles::{self, GAP, PADDING};
use crate::format;

pub const KEY_CONTEXT: &str = "WallhavenBrowser";

actions!(wallhaven_browser, [FocusSearch, DownloadSelected]);

/// Emitted when a download has added an image to the library.
pub struct LibraryChanged;

/// Decoded thumbnails kept in memory; well above the tiles visible at once.
const DECODED_THUMBNAILS: usize = 300;
/// Thumbnails downloaded at the same time.
const THUMBNAIL_BATCH: usize = 6;
/// How often download progress is redrawn.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

const ORIENTATIONS: [(Option<Orientation>, &str); 4] = [
    (None, "All"),
    (Some(Orientation::Landscape), "Landscape"),
    (Some(Orientation::Portrait), "Portrait"),
    (Some(Orientation::Square), "Square"),
];

const SORTS: [(SearchSort, &str); 6] = [
    (SearchSort::Newest, "Newest"),
    (SearchSort::Relevance, "Relevance"),
    (SearchSort::Views, "Most Viewed"),
    (SearchSort::Favorites, "Most Favorited"),
    (SearchSort::Top, "Top"),
    (SearchSort::Random, "Random"),
];

/// Where downloads go, when a library folder is available.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryLocation {
    pub root: PathBuf,
    pub database: PathBuf,
}

/// The state of the current search.
enum Status {
    /// The first page is loading; earlier results are gone.
    Searching,
    LoadingMore,
    Ready,
    Failed(SharedString),
}

enum Thumbnail {
    Ready(Arc<Path>),
    Failed,
}

/// Bytes received so far and the total, if known, written from the download
/// thread and read when drawing.
#[derive(Default)]
struct DownloadProgress {
    received: AtomicU64,
    /// `u64::MAX` while the size is unknown.
    total: AtomicU64,
}

impl DownloadProgress {
    /// The share received, from 0 to 100, once the size is known.
    fn percent(&self) -> Option<f32> {
        let total = self.total.load(Ordering::Relaxed);
        let received = self.received.load(Ordering::Relaxed);
        (total != u64::MAX && total > 0).then(|| received as f32 * 100. / total as f32)
    }
}

pub struct WallhavenBrowser {
    provider: Arc<dyn Provider>,
    library: Option<LibraryLocation>,
    thumbnail_cache: RemoteThumbnails,
    query_input: Entity<InputState>,
    query: SearchQuery,
    /// Results of every page loaded so far, without duplicates.
    wallpapers: Arc<Vec<RemoteWallpaper>>,
    page: u32,
    last_page: u32,
    total: u64,
    status: Status,
    /// Changes with every new search, so results of an older one are dropped.
    revision: u64,
    /// Keyed by wallpaper id; wallpapers without one are still loading.
    thumbnails: HashMap<String, Thumbnail>,
    /// Ids of the wallpapers the library already has.
    in_library: HashSet<String>,
    /// Downloads in progress, by wallpaper id.
    downloads: HashMap<String, Arc<DownloadProgress>>,
    /// The selected wallpaper's id, so it survives loading more pages.
    selected: Option<String>,
    width: Pixels,
    /// Columns of the frame being drawn, fixed at render time as in the library
    /// grid.
    columns: usize,
    scroll_handle: UniformListScrollHandle,
    focus_handle: FocusHandle,
    image_cache: Entity<LruImageCache>,
    _query_subscription: Subscription,
    _search_task: Option<Task<()>>,
    _thumbnail_task: Option<Task<()>>,
    _library_task: Option<Task<()>>,
    _progress_task: Option<Task<()>>,
}

impl WallhavenBrowser {
    /// Starts by showing the newest safe-for-work wallpapers.
    pub fn new(
        provider: Arc<dyn Provider>,
        library: Option<LibraryLocation>,
        thumbnails_dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(format!("Search {}", provider.name()))
        });
        // Enter searches and moves to the results, so arrow keys pick one.
        let query_subscription =
            cx.subscribe_in(&query_input, window, |this, input, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.query.text = input.read(cx).value().trim().to_owned();
                    this.search(cx);
                    window.focus(&this.focus_handle, cx);
                }
            });
        let mut browser = Self {
            provider,
            library,
            thumbnail_cache: RemoteThumbnails::new(thumbnails_dir),
            query_input,
            query: SearchQuery::default(),
            wallpapers: Arc::new(Vec::new()),
            page: 0,
            last_page: 0,
            total: 0,
            status: Status::Searching,
            revision: 0,
            thumbnails: HashMap::new(),
            in_library: HashSet::new(),
            downloads: HashMap::new(),
            selected: None,
            width: px(0.),
            columns: 1,
            scroll_handle: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            image_cache: LruImageCache::new(DECODED_THUMBNAILS, cx),
            _query_subscription: query_subscription,
            _search_task: None,
            _thumbnail_task: None,
            _library_task: None,
            _progress_task: None,
        };
        browser.refresh_library(cx);
        browser.search(cx);
        browser
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    #[cfg(test)]
    pub fn wallpapers(&self) -> &[RemoteWallpaper] {
        &self.wallpapers
    }

    #[cfg(test)]
    pub fn is_in_library(&self, id: &str) -> bool {
        self.in_library.contains(id)
    }

    #[cfg(test)]
    pub fn is_downloading(&self, id: &str) -> bool {
        self.downloads.contains_key(id)
    }

    /// Downloads go to `library`, or are unavailable without one.
    pub fn set_library(&mut self, library: Option<LibraryLocation>, cx: &mut Context<Self>) {
        if self.library != library {
            self.library = library;
            self.refresh_library(cx);
            cx.notify();
        }
    }

    /// Reads which wallpapers the library has from its index, which the
    /// caller keeps up to date.
    pub fn refresh_library(&mut self, cx: &mut Context<Self>) {
        let Some(library) = self.library.clone() else {
            self.in_library.clear();
            self._library_task = None;
            return;
        };
        let provider = self.provider.clone();
        self._library_task = Some(cx.spawn(async move |this, cx| {
            let ids = cx
                .background_spawn(async move {
                    let library = Library::open(&library.root, &library.database)?;
                    let prefix = format!("{}/", provider.id());
                    anyhow::Ok(
                        library
                            .image_sources(provider.as_ref())?
                            .into_values()
                            .filter_map(|source| {
                                Some(source.source().strip_prefix(&prefix)?.to_owned())
                            })
                            .collect::<HashSet<_>>(),
                    )
                })
                .await;
            // Without the index, nothing is marked; downloading still skips
            // wallpapers the library has.
            let Ok(ids) = ids else {
                return;
            };
            this.update(cx, |browser, cx| {
                browser.in_library = ids;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Starts a new search for the current query, replacing the results.
    fn search(&mut self, cx: &mut Context<Self>) {
        self.revision += 1;
        self.query.page = 1;
        self.query.seed = None;
        self.wallpapers = Arc::new(Vec::new());
        self.page = 0;
        self.last_page = 0;
        self.total = 0;
        self.selected = None;
        self.status = Status::Searching;
        self.scroll_handle.scroll_to_item(0, ScrollStrategy::Top);
        self.fetch_page(cx);
        cx.notify();
    }

    fn has_more_pages(&self) -> bool {
        self.page < self.last_page
    }

    /// Loads the next page of results, unless one is loading or there are no
    /// more.
    fn load_more(&mut self, cx: &mut Context<Self>) {
        let idle = matches!(self.status, Status::Ready | Status::Failed(_));
        if !idle || !self.has_more_pages() {
            return;
        }
        self.query.page = self.page + 1;
        self.status = Status::LoadingMore;
        self.fetch_page(cx);
        cx.notify();
    }

    /// Requests `self.query` and adds the results, if the search is still
    /// current when they arrive.
    fn fetch_page(&mut self, cx: &mut Context<Self>) {
        let provider = self.provider.clone();
        let query = self.query.clone();
        let revision = self.revision;
        self._search_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { provider.search(&query) })
                .await;
            this.update(cx, |browser, cx| {
                if browser.revision != revision {
                    return;
                }
                match result {
                    Ok(page) => {
                        let mut wallpapers = (*browser.wallpapers).clone();
                        let known: HashSet<String> =
                            wallpapers.iter().map(|w| w.id().to_owned()).collect();
                        // Pages shift as wallpapers are uploaded; skip repeats.
                        wallpapers.extend(
                            page.wallpapers
                                .into_iter()
                                .filter(|wallpaper| !known.contains(wallpaper.id())),
                        );
                        browser.wallpapers = Arc::new(wallpapers);
                        browser.page = page.page;
                        browser.last_page = page.last_page;
                        browser.total = page.total;
                        browser.query.seed = page.seed;
                        browser.status = Status::Ready;
                        browser.load_thumbnails(cx);
                    }
                    Err(error) => browser.status = Status::Failed(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Downloads missing thumbnails in display order, a batch at a time, and
    /// shows each batch as it completes.
    fn load_thumbnails(&mut self, cx: &mut Context<Self>) {
        let missing: Vec<RemoteWallpaper> = self
            .wallpapers
            .iter()
            .filter(|wallpaper| !self.thumbnails.contains_key(wallpaper.id()))
            .cloned()
            .collect();
        let provider = self.provider.clone();
        let cache = self.thumbnail_cache.clone();
        self._thumbnail_task = Some(cx.spawn(async move |this, cx| {
            for batch in missing.chunks(THUMBNAIL_BATCH) {
                let tasks: Vec<_> = batch
                    .iter()
                    .cloned()
                    .map(|wallpaper| {
                        let provider = provider.clone();
                        let cache = cache.clone();
                        cx.background_spawn(async move {
                            let result = cache.ensure(provider.as_ref(), &wallpaper);
                            (wallpaper.id().to_owned(), result)
                        })
                    })
                    .collect();
                let mut results = Vec::with_capacity(tasks.len());
                for task in tasks {
                    results.push(task.await);
                }
                let updated = this.update(cx, |browser, cx| {
                    for (id, result) in results {
                        let thumbnail = match result {
                            Ok(file) => Thumbnail::Ready(file.into()),
                            Err(_) => Thumbnail::Failed,
                        };
                        browser.thumbnails.insert(id, thumbnail);
                    }
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        }));
    }

    pub fn selected_ix(&self) -> Option<usize> {
        let selected = self.selected.as_deref()?;
        self.wallpapers
            .iter()
            .position(|wallpaper| wallpaper.id() == selected)
    }

    fn selected_wallpaper(&self) -> Option<&RemoteWallpaper> {
        self.wallpapers.get(self.selected_ix()?)
    }

    /// Selects the wallpaper at `ix` and scrolls it into view. Reaching the
    /// last row loads the next page, so the keyboard never runs out of
    /// results while there are more.
    pub fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(wallpaper) = self.wallpapers.get(ix) else {
            return;
        };
        self.selected = Some(wallpaper.id().to_owned());
        let row = ix / self.columns;
        self.scroll_handle
            .scroll_to_item(row, ScrollStrategy::Nearest);
        if row + 1 >= self.wallpapers.len().div_ceil(self.columns) {
            self.load_more(cx);
        }
        cx.notify();
    }

    fn move_selection(&mut self, offset: isize, cx: &mut Context<Self>) {
        if self.wallpapers.is_empty() {
            return;
        }
        let ix = match self.selected_ix() {
            Some(ix) => ix
                .saturating_add_signed(offset)
                .min(self.wallpapers.len() - 1),
            None => 0,
        };
        self.select(ix, cx);
    }

    fn set_orientation(&mut self, orientation: Option<Orientation>, cx: &mut Context<Self>) {
        if self.query.orientation != orientation {
            self.query.orientation = orientation;
            self.search(cx);
        }
    }

    fn set_sort(&mut self, sort: SearchSort, cx: &mut Context<Self>) {
        if self.query.sort != sort {
            self.query.sort = sort;
            self.search(cx);
        }
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus_handle = self.query_input.read(cx).focus_handle(cx);
        window.focus(&focus_handle, cx);
    }

    /// Downloads the selected wallpaper into the library, into the folder its
    /// download rules choose.
    pub fn download_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(library) = self.library.clone() else {
            return;
        };
        let Some(wallpaper) = self.selected_wallpaper().cloned() else {
            return;
        };
        let id = wallpaper.id().to_owned();
        if self.in_library.contains(&id) || self.downloads.contains_key(&id) {
            return;
        }
        let progress = Arc::new(DownloadProgress::default());
        progress.total.store(u64::MAX, Ordering::Relaxed);
        self.downloads.insert(id.clone(), progress.clone());
        self.redraw_while_downloading(cx);
        cx.notify();

        let provider = self.provider.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let mut library = Library::open(&library.root, &library.database)?;
                    library.download(
                        provider.as_ref(),
                        &wallpaper,
                        None,
                        &mut |received, total| {
                            progress.received.store(received, Ordering::Relaxed);
                            progress
                                .total
                                .store(total.unwrap_or(u64::MAX), Ordering::Relaxed);
                        },
                    )
                })
                .await;
            this.update_in(cx, |browser, window, cx| {
                browser.downloads.remove(&id);
                match result {
                    Ok(Downloaded::Saved(_) | Downloaded::AlreadyInLibrary(_)) => {
                        browser.in_library.insert(id);
                        cx.emit(LibraryChanged);
                    }
                    Err(error) => window.push_notification(
                        Notification::error(error.to_string())
                            .title(format!("Couldn't download {id}")),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Redraws regularly while downloads run, so their progress moves.
    fn redraw_while_downloading(&mut self, cx: &mut Context<Self>) {
        if self._progress_task.is_some() {
            return;
        }
        self._progress_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PROGRESS_INTERVAL).await;
                let running = this.update(cx, |browser, cx| {
                    let running = !browser.downloads.is_empty();
                    if running {
                        cx.notify();
                    } else {
                        browser._progress_task = None;
                    }
                    running
                });
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        }));
    }

    fn set_width(&mut self, width: Pixels, cx: &mut Context<Self>) {
        if self.width != width {
            self.width = width;
            cx.notify();
        }
    }
}

impl EventEmitter<LibraryChanged> for WallhavenBrowser {}

impl Render for WallhavenBrowser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.columns = tiles::columns(self.width, window.rem_size());
        let browser = cx.entity();

        v_flex()
            .id("wallhaven-browser")
            .track_focus(&self.focus_handle)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &SelectLeft, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectRight, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectUp, _, cx| {
                this.move_selection(-(this.columns as isize), cx)
            }))
            .on_action(cx.listener(|this, _: &SelectDown, _, cx| {
                this.move_selection(this.columns as isize, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.select(0, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                let last = this.wallpapers.len().saturating_sub(1);
                this.select(last, cx)
            }))
            .on_action(
                cx.listener(|this, _: &FocusSearch, window, cx| this.focus_search(window, cx)),
            )
            .on_action(cx.listener(|this, _: &DownloadSelected, window, cx| {
                this.download_selected(window, cx)
            }))
            .size_full()
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        // Measures the grid; the column count depends on its width.
                        canvas(
                            move |bounds, _, cx| {
                                browser.update(cx, |browser, cx| {
                                    browser.set_width(bounds.size.width, cx)
                                })
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(self.render_results(window, cx)),
            )
            .child(self.render_selection_bar(cx))
    }
}

impl WallhavenBrowser {
    /// The search field and the filters that shape the results, always
    /// visible so the current search is never a hidden state.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let orientation = self.query.orientation;
        let sort = self.query.sort;
        let sort_name = SORTS
            .iter()
            .find(|(candidate, _)| *candidate == sort)
            .map_or("", |(_, name)| *name);
        let browser = cx.entity();
        let count = match self.status {
            Status::Ready | Status::LoadingMore if self.total > 0 => Some(format::count(
                self.total as usize,
                "wallpaper",
                "wallpapers",
            )),
            _ => None,
        };

        h_flex()
            .flex_none()
            .px(rems(PADDING))
            .py_2()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Input::new(&self.query_input)
                    .id("wallhaven-search")
                    .w_64()
                    .small()
                    .prefix(Icon::new(IconName::Search).small())
                    .cleanable(true),
            )
            .child(
                ToggleGroup::new("wallhaven-orientation")
                    .segmented()
                    .outline()
                    .small()
                    .children(ORIENTATIONS.iter().map(|(candidate, name)| {
                        Toggle::new(*name)
                            .label(*name)
                            .checked(*candidate == orientation)
                    }))
                    .on_click(cx.listener(|this, checks: &Vec<bool>, _, cx| {
                        // A pick-one control, as in the library grid.
                        let current = ORIENTATIONS
                            .iter()
                            .position(|(candidate, _)| *candidate == this.query.orientation);
                        let picked = checks
                            .iter()
                            .enumerate()
                            .find(|(ix, checked)| **checked && Some(*ix) != current)
                            .map(|(ix, _)| ix);
                        if let Some(ix) = picked {
                            this.set_orientation(ORIENTATIONS[ix].0, cx);
                        }
                    })),
            )
            .child(div().flex_1())
            .when_some(count, |toolbar, count| {
                toolbar.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(count),
                )
            })
            .child(
                Button::new("wallhaven-sort")
                    .ghost()
                    .small()
                    .icon(IconName::SortDescending)
                    .label(sort_name)
                    .tooltip("Sort by")
                    .dropdown_menu(move |menu, _, _| {
                        SORTS.iter().fold(menu, |menu, (candidate, name)| {
                            let candidate = *candidate;
                            let browser = browser.clone();
                            menu.item(
                                PopupMenuItem::new(*name)
                                    .checked(candidate == sort)
                                    .on_click(move |_, _, cx| {
                                        browser.update(cx, |browser, cx| {
                                            browser.set_sort(candidate, cx)
                                        })
                                    }),
                            )
                        })
                    }),
            )
    }

    fn render_results(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.wallpapers.is_empty() {
            return match &self.status {
                Status::Searching | Status::LoadingMore => centered()
                    .gap_2()
                    .text_color(cx.theme().muted_foreground)
                    .child(Spinner::new())
                    .child("Searching…")
                    .into_any_element(),
                Status::Failed(message) => centered()
                    .gap_3()
                    .child("Couldn’t search Wallhaven")
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(message.clone()),
                    )
                    .child(
                        Button::new("retry-search")
                            .outline()
                            .small()
                            .label("Try Again")
                            .on_click(cx.listener(|this, _, _, cx| this.search(cx))),
                    )
                    .into_any_element(),
                Status::Ready => centered()
                    .child(if self.query.text.is_empty() {
                        "No wallpapers found".to_owned()
                    } else {
                        format!("No wallpapers match “{}”", self.query.text)
                    })
                    .into_any_element(),
            };
        }

        let rows = self.wallpapers.len().div_ceil(self.columns);
        div()
            .size_full()
            .child(
                image_cache(self.image_cache.clone()).size_full().child(
                    uniform_list(
                        "wallhaven-rows",
                        // One more row after the results, for loading more.
                        rows + 1,
                        cx.processor(move |browser, range: Range<usize>, window, cx| {
                            range
                                .map(|row| {
                                    if row < rows {
                                        browser.render_row(row, window, cx)
                                    } else {
                                        browser.render_more(cx)
                                    }
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .pt(window.rem_size() * PADDING)
                    .track_scroll(&self.scroll_handle)
                    .size_full(),
                ),
            )
            .child(Scrollbar::vertical(&self.scroll_handle))
            .into_any_element()
    }

    fn render_row(&self, row: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let rem = window.rem_size();
        let columns = self.columns;
        let tile = tiles::tile_size(self.width, columns, rem);
        let start = (row * columns).min(self.wallpapers.len());
        let end = (start + columns).min(self.wallpapers.len());
        h_flex()
            .px(rem * PADDING)
            .pb(rem * GAP)
            .gap(rem * GAP)
            .children(
                self.wallpapers[start..end]
                    .iter()
                    .enumerate()
                    .map(|(column, wallpaper)| {
                        self.render_tile(wallpaper, start + column, tile, rem, cx)
                    }),
            )
            .into_any_element()
    }

    fn render_tile(
        &self,
        wallpaper: &RemoteWallpaper,
        ix: usize,
        tile: Size<Pixels>,
        rem: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let id = wallpaper.id();
        let selected = self.selected.as_deref() == Some(id);
        let content = match self.thumbnails.get(id) {
            Some(Thumbnail::Ready(file)) => img(file.clone())
                .size_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            Some(Thumbnail::Failed) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::TriangleAlert).small())
                .into_any_element(),
            None => div().into_any_element(),
        };
        let progress = self.downloads.get(id).map(|progress| progress.percent());

        let picture = div()
            .size_full()
            .rounded(theme.radius)
            .overflow_hidden()
            .bg(theme.muted)
            .child(content)
            .when(self.in_library.contains(id), |picture| {
                picture.child(tiles::picture_badge("In library", cx))
            })
            .when_some(progress, |picture, percent| {
                picture.child(
                    div().absolute().left_2().right_2().bottom_2().child(
                        Progress::new(ElementId::Name(format!("download-progress-{id}").into()))
                            .loading(percent.is_none())
                            .value(percent.unwrap_or(0.))
                            .accessibility_label(format!("Downloading {id}")),
                    ),
                )
            });

        div()
            .id(ElementId::Name(format!("wallpaper-{id}").into()))
            .test_support()
            .relative()
            .flex_none()
            .w(tile.width)
            .h(tile.height)
            .child(picture)
            .when(selected, |tile| tile.child(tiles::selection_ring(rem, cx)))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                window.focus(&this.focus_handle, cx);
                this.select(ix, cx);
            }))
            .into_any_element()
    }

    /// The row after the results: loading, a way to load more, or the end.
    fn render_more(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let row = h_flex()
            .h_10()
            .px(rems(PADDING))
            .gap_2()
            .justify_center()
            .text_sm()
            .text_color(theme.muted_foreground);
        match &self.status {
            Status::Searching | Status::LoadingMore => row
                .child(Spinner::new())
                .child("Loading more…")
                .into_any_element(),
            Status::Failed(message) => row
                .child(message.clone())
                .child(
                    Button::new("retry-more")
                        .outline()
                        .small()
                        .label("Try Again")
                        .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
                )
                .into_any_element(),
            Status::Ready if self.has_more_pages() => row
                .child(
                    Button::new("load-more")
                        .outline()
                        .small()
                        .label("Load More")
                        .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
                )
                .into_any_element(),
            Status::Ready => row.child("End of results").into_any_element(),
        }
    }

    /// What the selected wallpaper is, where to see it on Wallhaven, and how
    /// to download it.
    fn render_selection_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let bar = h_flex()
            .flex_none()
            .h_10()
            .px(rems(PADDING))
            .gap_3()
            .border_t_1()
            .border_color(theme.border)
            .text_sm();
        let Some(wallpaper) = self.selected_wallpaper() else {
            return bar.child(
                div()
                    .text_color(theme.muted_foreground)
                    .child("Select a wallpaper to see it or download it."),
            );
        };
        let id = wallpaper.id().to_owned();
        let mut details = vec![format::dimensions(wallpaper.width(), wallpaper.height())];
        if let Some(size) = wallpaper.file_size() {
            details.push(format::file_size(size));
        }
        if let Some(category) = wallpaper.category() {
            details.push(category.to_owned());
        }

        let action = if self.in_library.contains(&id) {
            div()
                .text_color(theme.muted_foreground)
                .child("In library")
                .into_any_element()
        } else if let Some(progress) = self.downloads.get(&id) {
            h_flex()
                .gap_2()
                .text_color(theme.muted_foreground)
                .child(match progress.percent() {
                    Some(percent) => format!("Downloading… {}%", percent.floor()),
                    None => "Downloading…".to_owned(),
                })
                .into_any_element()
        } else {
            let available = self.library.is_some();
            Button::new("download")
                .small()
                .label("Download")
                .disabled(!available)
                .tooltip(if available {
                    "Download into the library (D)"
                } else {
                    "Choose a library folder to download into"
                })
                .on_click(cx.listener(|this, _, window, cx| this.download_selected(window, cx)))
                .into_any_element()
        };

        bar.child(
            div()
                .min_w_0()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(details.join(" · ")),
        )
        .child(
            Link::new("open-on-wallhaven")
                .href(wallpaper.page_url().to_owned())
                .child(format!("{} on wallhaven.cc", wallpaper.id())),
        )
        .child(div().flex_1())
        .child(action)
    }
}

/// A full-size column centering its contents, for states without results.
fn centered() -> Div {
    v_flex().size_full().items_center().justify_center()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::Arc;

    use backdrop_core::library::Library;
    use backdrop_core::paths::AppPaths;
    use backdrop_core::providers::ContentRating;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext, px, size};

    use super::super::test_provider::FakeProvider;
    use super::{LibraryChanged, LibraryLocation, WallhavenBrowser};

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        provider: Arc<FakeProvider>,
        window: AnyWindowHandle,
        browser: Entity<WallhavenBrowser>,
        /// How many times the browser said the library changed.
        changes: Rc<Cell<usize>>,
    }

    /// A browser over `pages` of results, downloading into a scanned library
    /// that already holds `existing` images.
    fn open_browser(
        pages: Vec<Vec<&'static str>>,
        existing: &[&str],
        cx: &mut TestAppContext,
    ) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir(&root).unwrap();
        for name in existing {
            image::RgbImage::new(20, 10).save(root.join(name)).unwrap();
        }
        let paths = AppPaths::new(
            dir.path().join("config"),
            dir.path().join("data"),
            dir.path().join("cache"),
        );
        Library::open(&root, &paths.database_file())
            .unwrap()
            .scan()
            .unwrap();
        let provider = Arc::new(FakeProvider::new(pages));
        let location = LibraryLocation {
            root: root.clone(),
            database: paths.database_file(),
        };

        cx.update(|cx| {
            gpui_kit::init(cx);
            super::super::bind_keys(cx);
        });
        let changes = Rc::new(Cell::new(0));
        let mut browser = None;
        let window_provider = provider.clone();
        let window_changes = changes.clone();
        let handle = cx.open_window(size(px(800.), px(600.)), |window, cx| {
            let view = cx.new(|cx| {
                WallhavenBrowser::new(
                    window_provider,
                    Some(location),
                    paths.remote_thumbnails_dir(),
                    window,
                    cx,
                )
            });
            cx.subscribe(&view, move |_, _, _: &LibraryChanged, _| {
                window_changes.set(window_changes.get() + 1)
            })
            .detach();
            browser = Some(view.clone());
            Root::new(view, window, cx)
        });
        cx.run_until_parked();
        let browser = browser.unwrap();
        let window: AnyWindowHandle = handle.into();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            let focus_handle = browser.read(cx).focus_handle().clone();
            window.focus(&focus_handle, cx);
        })
        .unwrap();
        Fixture {
            _dir: dir,
            root,
            provider,
            window,
            browser,
            changes,
        }
    }

    fn ids(fixture: &Fixture, cx: &mut TestAppContext) -> Vec<String> {
        cx.update(|cx| {
            fixture
                .browser
                .read(cx)
                .wallpapers()
                .iter()
                .map(|wallpaper| wallpaper.id().to_owned())
                .collect()
        })
    }

    fn press(fixture: &Fixture, key: &str, cx: &mut TestAppContext) {
        cx.update_window(fixture.window, |_, window, cx| window.press(key, cx))
            .unwrap();
        cx.run_until_parked();
    }

    #[gpui_kit::test]
    fn opens_with_the_newest_safe_for_work_wallpapers(cx: &mut TestAppContext) {
        let fixture = open_browser(vec![vec!["aaa111", "bbb222", "ccc333"]], &[], cx);

        assert_eq!(ids(&fixture, cx), ["aaa111", "bbb222", "ccc333"]);
        let [search] = fixture.provider.searches().try_into().unwrap();
        assert_eq!(search.effective_ratings(), [ContentRating::Sfw]);
        assert_eq!(search.page, 1);
    }

    #[gpui_kit::test]
    fn the_keyboard_selects_and_downloads_into_the_library(cx: &mut TestAppContext) {
        let fixture = open_browser(vec![vec!["aaa111", "bbb222", "ccc333"]], &[], cx);

        press(&fixture, "right", cx);
        press(&fixture, "right", cx);
        press(&fixture, "d", cx);

        assert!(fixture.root.join("fake-bbb222.png").is_file());
        cx.update(|cx| {
            let browser = fixture.browser.read(cx);
            assert!(browser.is_in_library("bbb222"));
            assert!(!browser.is_downloading("bbb222"));
        });
        assert_eq!(fixture.changes.get(), 1);
    }

    #[gpui_kit::test]
    fn wallpapers_in_the_library_are_marked_and_not_downloaded_again(cx: &mut TestAppContext) {
        let fixture = open_browser(vec![vec!["aaa111", "bbb222"]], &["fake-aaa111.png"], cx);

        cx.update(|cx| {
            let browser = fixture.browser.read(cx);
            assert!(browser.is_in_library("aaa111"));
            assert!(!browser.is_in_library("bbb222"));
        });
        press(&fixture, "right", cx);
        press(&fixture, "d", cx);

        assert_eq!(fixture.changes.get(), 0);
    }

    #[gpui_kit::test]
    fn reaching_the_last_row_loads_the_next_page(cx: &mut TestAppContext) {
        let fixture = open_browser(vec![vec!["aaa111", "bbb222"], vec!["ccc333"]], &[], cx);

        // Both results fit in one row, so selecting the first reaches it.
        press(&fixture, "right", cx);

        assert_eq!(ids(&fixture, cx), ["aaa111", "bbb222", "ccc333"]);
        let searches = fixture.provider.searches();
        assert_eq!(searches.len(), 2);
        assert_eq!(searches[1].page, 2);
    }

    #[gpui_kit::test]
    fn enter_in_the_search_field_searches_and_moves_to_the_results(cx: &mut TestAppContext) {
        let fixture = open_browser(vec![vec!["aaa111", "bbb222"]], &[], cx);

        press(&fixture, "ctrl-f", cx);
        cx.update_window(fixture.window, |_, window, cx| {
            window.input("mountains", cx)
        })
        .unwrap();
        press(&fixture, "enter", cx);
        press(&fixture, "right", cx);

        let searches = fixture.provider.searches();
        assert_eq!(searches.len(), 2);
        assert_eq!(searches[1].text, "mountains");
        assert_eq!(
            cx.update(|cx| fixture.browser.read(cx).selected_ix()),
            Some(0)
        );
    }
}
