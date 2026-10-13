//! The grid of library images.
//!
//! Rows are virtualized with `uniform_list`, so only visible tiles exist. The
//! column count follows the grid's measured width. Thumbnails are generated in
//! the background in display order; a tile shows a placeholder until its
//! thumbnail is ready.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use backdrop_core::desktop::WallpaperBackend;
use backdrop_core::image::ImageFile;
use backdrop_core::image::Orientation;
use backdrop_core::library::{Library, Trash};
use backdrop_core::metadata::same_tag;
use backdrop_core::paths::AppPaths;
use backdrop_core::query::{ImageFilter, ImageMarks, ImageOrder, TagCount};
use backdrop_core::thumbnails::Thumbnails;
use gpui_kit::component::button::{
    Button, ButtonVariant, ButtonVariants as _, Toggle, ToggleGroup, ToggleVariants as _,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::image_cache::LruImageCache;
use super::tiles::{self, GAP, PADDING};
use crate::format;

pub const KEY_CONTEXT: &str = "LibraryGrid";

actions!(
    library_grid,
    [
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectFirst,
        SelectLast,
        OpenSelected,
        SetWallpaper,
        DeleteSelected,
        ToggleFavorite,
        FocusTagFilter,
        Dismiss,
        ExtendLeft,
        ExtendRight,
        ExtendUp,
        ExtendDown,
        SelectAll
    ]
);

/// Emitted when the user asks to see the selected image in full.
pub struct OpenImage;

/// Tag suggestions shown while typing in the tag filter.
const TAG_SUGGESTIONS: usize = 8;

/// Decoded thumbnails kept in memory; well above the tiles visible at once.
const DECODED_THUMBNAILS: usize = 300;

enum Thumbnail {
    Ready(Arc<Path>),
    Failed,
}

pub struct LibraryGrid {
    root: PathBuf,
    paths: AppPaths,
    /// Every image in the library, sorted by `order`.
    all: Vec<ImageFile>,
    /// Whether `all` has been read from the index yet.
    loaded: bool,
    /// The images shown: `all` narrowed by `filter`.
    images: Arc<Vec<ImageFile>>,
    order: ImageOrder,
    filter: ImageFilter,
    /// Favorites and tags, for the filter and the tiles.
    marks: ImageMarks,
    /// Keyed by image path; images without an entry are still being prepared.
    thumbnails: HashMap<PathBuf, Thumbnail>,
    /// The current image, by path so it survives reordering: where keyboard
    /// movement starts, and what Enter opens.
    selected: Option<PathBuf>,
    /// Every selected image. Plain navigation selects just the current image;
    /// ctrl-click, shift-click, shift with the arrows, and ctrl-a select more.
    selection: HashSet<PathBuf>,
    /// Where a shift-selected range starts.
    anchor: Option<PathBuf>,
    /// The desktop shell, once detected; `None` while detecting or when no
    /// supported shell runs.
    desktop: Option<Arc<dyn WallpaperBackend>>,
    /// Where deleted images go.
    trash: Arc<dyn Trash>,
    /// The library image shown as the desktop wallpaper, if any.
    current_wallpaper: Option<PathBuf>,
    /// A request to set the wallpaper is in progress.
    setting_wallpaper: bool,
    /// Width of the grid from the last layout, used to pick the column count.
    width: Pixels,
    /// Columns of the frame being drawn. Fixed at render time: the width can
    /// change while the frame is laid out, and the row count already depends
    /// on this value.
    columns: usize,
    scroll_handle: UniformListScrollHandle,
    focus_handle: FocusHandle,
    image_cache: Entity<LruImageCache>,
    _load_task: Task<()>,
    _thumbnail_task: Option<Task<()>>,
    /// The latest tag save. Each save waits for the one before, so quick
    /// edits reach the metadata file in the order they were made.
    tag_save: Option<Task<()>>,
    /// The text typed to find a tag to filter by.
    tag_query: Entity<InputState>,
    _tag_query_subscription: Subscription,
}

impl LibraryGrid {
    pub fn new(
        root: PathBuf,
        paths: AppPaths,
        trash: Arc<dyn Trash>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tag_query = cx.new(|cx| InputState::new(window, cx).placeholder("Filter by tag…"));
        let tag_query_subscription = cx.subscribe_in(
            &tag_query,
            window,
            |grid, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => grid.add_top_suggestion(window, cx),
                // Suggestions follow what is typed.
                InputEvent::Change => cx.notify(),
                _ => {}
            },
        );
        let load_task = Self::load(&root, &paths, ImageOrder::default(), cx);
        Self {
            root,
            paths,
            all: Vec::new(),
            loaded: false,
            images: Arc::new(Vec::new()),
            order: ImageOrder::default(),
            filter: ImageFilter::default(),
            marks: ImageMarks::default(),
            thumbnails: HashMap::new(),
            selected: None,
            selection: HashSet::new(),
            anchor: None,
            desktop: None,
            trash,
            current_wallpaper: None,
            setting_wallpaper: false,
            width: px(0.),
            columns: 1,
            scroll_handle: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            image_cache: LruImageCache::new(DECODED_THUMBNAILS, cx),
            _load_task: load_task,
            _thumbnail_task: None,
            tag_save: None,
            tag_query,
            _tag_query_subscription: tag_query_subscription,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    /// Reads the images, favorites, and tags from the index, which the caller has
    /// just brought up to date.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self._load_task = Self::load(&self.root, &self.paths, self.order, cx);
    }

    fn load(root: &Path, paths: &AppPaths, order: ImageOrder, cx: &mut Context<Self>) -> Task<()> {
        let root = root.to_path_buf();
        let database = paths.database_file();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn(async move {
                    let library = Library::open(&root, &database)?;
                    anyhow::Ok((library.images(order)?, library.marks()?))
                })
                .await;
            // A failed load leaves the grid as it was; the scan that preceded
            // it has already reported problems with the index.
            let Ok((images, marks)) = loaded else {
                return;
            };
            this.update(cx, |grid, cx| {
                grid.all = images;
                grid.loaded = true;
                grid.marks = marks;
                grid.apply_filter(cx);
            })
            .ok();
        })
    }

    /// Shows the images in this order, keeping the filter.
    pub fn set_order(&mut self, order: ImageOrder, cx: &mut Context<Self>) {
        self.order = order;
        order.sort(&mut self.all);
        self.apply_filter(cx);
        self.scroll_handle.scroll_to_item(0, ScrollStrategy::Top);
    }

    pub fn set_filter(&mut self, filter: ImageFilter, cx: &mut Context<Self>) {
        self.filter = filter;
        self.apply_filter(cx);
        self.scroll_handle.scroll_to_item(0, ScrollStrategy::Top);
    }

    /// Recomputes the shown images. The current image is kept by path; while
    /// it is filtered out, no image is current. Images the filter hides leave
    /// the selection, so actions on it never reach images out of sight.
    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let images = self
            .all
            .iter()
            .filter(|image| self.filter.matches(image, &self.marks))
            .cloned()
            .collect();
        let images: Vec<ImageFile> = images;
        let shown: HashSet<&Path> = images.iter().map(ImageFile::path).collect();
        self.selection.retain(|path| shown.contains(path.as_path()));
        self.images = Arc::new(images);
        self.generate_thumbnails(cx);
        cx.notify();
    }

    /// Creates missing thumbnails of the shown images in display order, a few
    /// at a time on the background executor, and shows each batch as it
    /// completes.
    fn generate_thumbnails(&mut self, cx: &mut Context<Self>) {
        let images: Vec<ImageFile> = self
            .images
            .iter()
            .filter(|image| !self.thumbnails.contains_key(image.path()))
            .cloned()
            .collect();
        let root = self.root.clone();
        let thumbnails = Thumbnails::new(self.paths.thumbnails_dir());
        let batch_size = std::thread::available_parallelism().map_or(4, |n| n.get()) * 2;
        self._thumbnail_task = Some(cx.spawn(async move |this, cx| {
            for batch in images.chunks(batch_size) {
                let tasks: Vec<_> = batch
                    .iter()
                    .cloned()
                    .map(|image| {
                        let root = root.clone();
                        let thumbnails = thumbnails.clone();
                        cx.background_spawn(async move {
                            let result = thumbnails.ensure(&root, &image);
                            (image.path().to_path_buf(), result)
                        })
                    })
                    .collect();
                let mut results = Vec::with_capacity(tasks.len());
                for task in tasks {
                    results.push(task.await);
                }
                let updated = this.update(cx, |grid, cx| {
                    for (path, result) in results {
                        let thumbnail = match result {
                            Ok(file) => Thumbnail::Ready(file.into()),
                            Err(_) => Thumbnail::Failed,
                        };
                        grid.thumbnails.insert(path, thumbnail);
                    }
                    cx.notify();
                });
                if updated.is_err() {
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

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Uses `desktop` to show and set wallpapers; `None` when no supported
    /// shell runs.
    pub fn set_desktop(
        &mut self,
        desktop: Option<Arc<dyn WallpaperBackend>>,
        cx: &mut Context<Self>,
    ) {
        self.desktop = desktop;
        self.current_wallpaper = None;
        self.refresh_current_wallpaper(cx);
        cx.notify();
    }

    /// Whether a supported desktop shell is running, so wallpapers can be set.
    pub fn has_desktop(&self) -> bool {
        self.desktop.is_some()
    }

    pub fn is_current_wallpaper(&self, path: &Path) -> bool {
        self.current_wallpaper.as_deref() == Some(path)
    }

    pub fn is_setting_wallpaper(&self) -> bool {
        self.setting_wallpaper
    }

    /// Asks the desktop shell which image is the wallpaper. The shell can't
    /// report changes, so this runs again when the window becomes active.
    pub fn refresh_current_wallpaper(&mut self, cx: &mut Context<Self>) {
        let Some(desktop) = self.desktop.clone() else {
            return;
        };
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let current = cx
                .background_spawn(async move {
                    let path = desktop.current_wallpaper().ok()??;
                    // The library root is stored resolved, so resolve this too.
                    let path = std::fs::canonicalize(path).ok()?;
                    Some(path.strip_prefix(&root).ok()?.to_path_buf())
                })
                .await;
            this.update(cx, |grid, cx| {
                if grid.current_wallpaper != current {
                    grid.current_wallpaper = current;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Images in the library, whether or not the filter shows them; `None`
    /// until they have been read.
    pub fn total(&self) -> Option<usize> {
        self.loaded.then_some(self.all.len())
    }

    /// Asks whether to move the selected images to the trash.
    pub fn confirm_delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.selected_paths();
        match paths.as_slice() {
            [] => {}
            [path] => self.confirm_delete(path.clone(), window, cx),
            _ => self.confirm_delete_several(paths, window, cx),
        }
    }

    /// Asks whether to move the current image to the trash, whatever else is
    /// selected: what the detail view deletes.
    pub fn confirm_delete_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_ix().is_some()
            && let Some(path) = self.selected.clone()
        {
            self.confirm_delete(path, window, cx);
        }
    }

    fn confirm_delete(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned();
        let title: SharedString = format!("Move “{name}” to Trash?").into();
        let description: SharedString = if self.is_current_wallpaper(&path) {
            "It’s your current wallpaper. You can restore it from the trash.".into()
        } else {
            "You can restore it from the trash.".into()
        };
        let grid = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let grid = grid.clone();
            let path = path.clone();
            alert
                .title(title.clone())
                .description(description.clone())
                .confirm()
                .ok_text("Move to Trash")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    grid.update(cx, |grid, cx| {
                        grid.delete_images(vec![path.clone()], window, cx)
                    })
                    .ok();
                    true
                })
        });
    }

    fn confirm_delete_several(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title: SharedString = format!(
            "Move {} to Trash?",
            format::count(paths.len(), "image", "images")
        )
        .into();
        let description: SharedString = if paths.iter().any(|path| self.is_current_wallpaper(path))
        {
            "One of them is your current wallpaper. You can restore them from the trash.".into()
        } else {
            "You can restore them from the trash.".into()
        };
        let grid = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let grid = grid.clone();
            let paths = paths.clone();
            alert
                .title(title.clone())
                .description(description.clone())
                .confirm()
                .ok_text("Move to Trash")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    grid.update(cx, |grid, cx| grid.delete_images(paths.clone(), window, cx))
                        .ok();
                    true
                })
        });
    }

    /// Moves the images at `paths` to the trash and takes them out of the grid.
    /// Images the trash refuses stay, and an error names how many.
    fn delete_images(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let database = self.paths.database_file();
        let trash = self.trash.clone();
        let count = paths.len();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    Library::open(&root, &database)?.delete_images(&paths, trash.as_ref())
                })
                .await;
            this.update_in(cx, |grid, window, cx| {
                let (title, message) = match result {
                    Ok(deletion) => {
                        grid.remove_from_view(deletion.deleted(), cx);
                        let Some((_, message)) = deletion.failed().first() else {
                            return;
                        };
                        let failed = deletion.failed().len();
                        let title = if count == 1 {
                            "Couldn't move the image to the trash".to_owned()
                        } else {
                            format!(
                                "Couldn't move {} of {} images to the trash",
                                format::grouped(failed),
                                format::grouped(count)
                            )
                        };
                        (title, message.clone())
                    }
                    Err(error) => (
                        "Couldn't move the images to the trash".to_owned(),
                        error.to_string(),
                    ),
                };
                window.push_notification(Notification::error(message).title(title), cx);
            })
            .ok();
        })
        .detach();
    }

    /// Takes deleted images out of the grid. If the current image went, the
    /// image that took the place of the first one deleted becomes current.
    fn remove_from_view(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let removed: HashSet<&PathBuf> = paths.iter().collect();
        let first = self
            .images
            .iter()
            .position(|image| removed.contains(&image.path().to_path_buf()));
        self.all
            .retain(|image| !removed.contains(&image.path().to_path_buf()));
        for path in paths {
            self.marks.remove(path);
            self.thumbnails.remove(path);
            self.selection.remove(path);
        }
        self.apply_filter(cx);
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| removed.contains(selected))
        {
            self.selected = None;
            if let Some(ix) = first.filter(|_| !self.images.is_empty()) {
                self.select(ix.min(self.images.len() - 1), cx);
            }
        }
        cx.notify();
    }

    /// Shows the selected image as the desktop wallpaper.
    pub fn set_selected_as_wallpaper(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(desktop), Some(path)) = (self.desktop.clone(), self.selected.clone()) else {
            return;
        };
        if self.setting_wallpaper || self.is_current_wallpaper(&path) {
            return;
        }
        self.setting_wallpaper = true;
        cx.notify();
        let file = self.root.join(&path);
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { desktop.set_wallpaper(&file) })
                .await;
            this.update_in(cx, |grid, window, cx| {
                grid.setting_wallpaper = false;
                match result {
                    Ok(()) => grid.current_wallpaper = Some(path),
                    Err(error) => window.push_notification(
                        Notification::error(error.to_string()).title("Couldn't set the wallpaper"),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The images in display order.
    pub fn images(&self) -> &Arc<Vec<ImageFile>> {
        &self.images
    }

    pub fn is_favorite(&self, path: &Path) -> bool {
        self.marks.is_favorite(path)
    }

    /// The cached thumbnail of the image at `path`, once it exists.
    pub fn thumbnail(&self, path: &Path) -> Option<Arc<Path>> {
        match self.thumbnails.get(path)? {
            Thumbnail::Ready(file) => Some(file.clone()),
            Thumbnail::Failed => None,
        }
    }

    pub fn selected_ix(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.images
            .iter()
            .position(|image| image.path() == selected)
    }

    /// Makes the image at `ix` current and the only one selected, and scrolls
    /// it into view.
    pub fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(image) = self.images.get(ix) else {
            return;
        };
        let path = image.path().to_path_buf();
        self.selection = HashSet::from([path.clone()]);
        self.anchor = Some(path.clone());
        self.selected = Some(path);
        self.scroll_handle
            .scroll_to_item(ix / self.columns, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// Makes the image at `ix` current, and adds it to the selection or takes
    /// it out, as ctrl-click does.
    pub fn toggle_selected(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(path) = self.images.get(ix).map(|image| image.path().to_path_buf()) else {
            return;
        };
        if !self.selection.remove(&path) {
            self.selection.insert(path.clone());
        }
        self.anchor = Some(path.clone());
        self.selected = Some(path);
        cx.notify();
    }

    /// Selects the images from the anchor to `ix` and makes `ix` current, as
    /// shift-click does. Without an anchor, the range starts at the current
    /// image.
    pub fn select_range(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(path) = self.images.get(ix).map(|image| image.path().to_path_buf()) else {
            return;
        };
        let start = self
            .anchor
            .as_ref()
            .or(self.selected.as_ref())
            .and_then(|anchor| self.images.iter().position(|image| image.path() == anchor))
            .unwrap_or(ix);
        let range = start.min(ix)..=start.max(ix);
        self.selection = self.images[range]
            .iter()
            .map(|image| image.path().to_path_buf())
            .collect();
        if self.anchor.is_none() {
            self.anchor = Some(self.images[start].path().to_path_buf());
        }
        self.selected = Some(path);
        self.scroll_handle
            .scroll_to_item(ix / self.columns, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// Extends the range by moving the current image `offset` places.
    fn extend_selection(&mut self, offset: isize, cx: &mut Context<Self>) {
        if self.images.is_empty() {
            return;
        }
        let ix = match self.selected_ix() {
            Some(ix) => ix.saturating_add_signed(offset).min(self.images.len() - 1),
            None => 0,
        };
        self.select_range(ix, cx);
    }

    /// Selects every shown image.
    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.selection = self
            .images
            .iter()
            .map(|image| image.path().to_path_buf())
            .collect();
        if self.selected_ix().is_none() {
            self.selected = self.images.first().map(|image| image.path().to_path_buf());
        }
        cx.notify();
    }

    /// Shrinks the selection to the current image.
    fn collapse_selection(&mut self, cx: &mut Context<Self>) {
        self.selection = self.selected.iter().cloned().collect();
        self.anchor = self.selected.clone();
        cx.notify();
    }

    /// How many shown images are selected.
    #[cfg(test)]
    pub fn selection_count(&self) -> usize {
        self.selection.len()
    }

    #[cfg(test)]
    pub fn is_in_selection(&self, path: &Path) -> bool {
        self.selection.contains(path)
    }

    /// The selected images, in display order.
    fn selected_paths(&self) -> Vec<PathBuf> {
        self.images
            .iter()
            .filter(|image| self.selection.contains(image.path()))
            .map(|image| image.path().to_path_buf())
            .collect()
    }

    /// Moves the selection by `offset` images, staying within the library.
    /// With nothing selected, any move selects the first image.
    fn move_selection(&mut self, offset: isize, cx: &mut Context<Self>) {
        if self.images.is_empty() {
            return;
        }
        let ix = match self.selected_ix() {
            Some(ix) => ix.saturating_add_signed(offset).min(self.images.len() - 1),
            None => 0,
        };
        self.select(ix, cx);
    }

    pub fn toggle_favorite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone() else {
            return;
        };
        let favorite = !self.marks.is_favorite(&path);
        // Shown at once; undone below if saving fails.
        self.set_favorite_shown(&path, favorite, cx);
        let root = self.root.clone();
        let database = self.paths.database_file();
        cx.spawn_in(window, async move |this, cx| {
            let saved_path = path.clone();
            let result = cx
                .background_spawn(async move {
                    Library::open(&root, &database)?.set_favorite(&saved_path, favorite)
                })
                .await;
            if let Err(error) = result {
                this.update_in(cx, |grid, window, cx| {
                    grid.set_favorite_shown(&path, !favorite, cx);
                    window.push_notification(
                        Notification::error(error.to_string())
                            .title("Couldn't change the favorite"),
                        cx,
                    );
                })
                .ok();
            }
        })
        .detach();
    }

    /// The user's tags of the image at `path`.
    pub fn tags(&self, path: &Path) -> &[String] {
        self.marks.tags(path)
    }

    /// Replaces the user tags of the image at `path`. Shown at once, saved in
    /// the background, and undone if saving fails.
    pub fn set_tags(
        &mut self,
        path: PathBuf,
        tags: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous = self.marks.tags(&path).to_vec();
        self.set_tags_shown(&path, tags.clone(), cx);
        let root = self.root.clone();
        let database = self.paths.database_file();
        let before = self.tag_save.take();
        let save = cx.spawn_in(window, async move |this, cx| {
            if let Some(before) = before {
                before.await;
            }
            let saved_path = path.clone();
            let result = cx
                .background_spawn(async move {
                    Library::open(&root, &database)?.set_tags(&saved_path, &tags)
                })
                .await;
            this.update_in(cx, |grid, window, cx| match result {
                // Shows the tags as stored, normalized.
                Ok(stored) => grid.set_tags_shown(&path, stored, cx),
                Err(error) => {
                    grid.set_tags_shown(&path, previous, cx);
                    window.push_notification(
                        Notification::error(error.to_string()).title("Couldn’t change the tags"),
                        cx,
                    );
                }
            })
            .ok();
        });
        self.tag_save = Some(save);
    }

    /// Every tag of the library's images, user and provider, most used
    /// first. Counted from what the grid holds, so it follows edits at once.
    pub fn tag_counts(&self) -> Vec<TagCount> {
        self.marks.tag_counts(self.all.iter().map(ImageFile::path))
    }

    /// Tags containing the typed text that aren't in the filter yet.
    fn tag_suggestions(&self, cx: &App) -> Vec<TagCount> {
        let typed = self.tag_query.read(cx).value().trim().to_lowercase();
        if typed.is_empty() {
            return Vec::new();
        }
        let selected = self.filter.tags();
        self.tag_counts()
            .into_iter()
            .filter(|count| count.name().to_lowercase().contains(&typed))
            .filter(|count| !selected.iter().any(|tag| same_tag(tag, count.name())))
            .take(TAG_SUGGESTIONS)
            .collect()
    }

    /// Narrows the grid to images that also have `tag`, and clears the field.
    fn add_filter_tag(&mut self, tag: String, window: &mut Window, cx: &mut Context<Self>) {
        let mut tags = self.filter.tags().to_vec();
        if !tags.iter().any(|existing| same_tag(existing, &tag)) {
            tags.push(tag);
            let filter = self.filter.clone().with_tags(tags);
            self.set_filter(filter, cx);
        }
        self.tag_query
            .update(cx, |query, cx| query.set_value("", window, cx));
    }

    fn add_top_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(top) = self.tag_suggestions(cx).into_iter().next() {
            self.add_filter_tag(top.name().to_owned(), window, cx);
        }
    }

    fn remove_filter_tag(&mut self, tag: &str, cx: &mut Context<Self>) {
        let tags = self
            .filter
            .tags()
            .iter()
            .filter(|existing| !same_tag(existing, tag))
            .cloned()
            .collect();
        let filter = self.filter.clone().with_tags(tags);
        self.set_filter(filter, cx);
    }

    fn focus_tag_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus_handle = self.tag_query.read(cx).focus_handle(cx);
        window.focus(&focus_handle, cx);
    }

    /// Escape: in the tag field, clears what is typed, then returns to the
    /// images; among the images, shrinks the selection to the current image.
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.tag_query.read(cx);
        if !query.focus_handle(cx).is_focused(window) {
            self.collapse_selection(cx);
        } else if !query.value().is_empty() {
            self.tag_query
                .update(cx, |query, cx| query.set_value("", window, cx));
        } else {
            window.focus(&self.focus_handle, cx);
        }
    }

    #[cfg(test)]
    pub fn filter(&self) -> &ImageFilter {
        &self.filter
    }

    fn set_tags_shown(&mut self, path: &Path, tags: Vec<String>, cx: &mut Context<Self>) {
        self.marks.set_tags(path, tags);
        if !self.filter.tags().is_empty() {
            self.apply_filter(cx);
        }
        cx.notify();
    }

    /// Shows the provider tags of the image at `path`, which was just linked
    /// to its source.
    pub fn set_provider_tags(&mut self, path: &Path, tags: Vec<String>, cx: &mut Context<Self>) {
        self.marks.set_provider_tags(path, tags);
        if !self.filter.tags().is_empty() {
            self.apply_filter(cx);
        }
        cx.notify();
    }

    fn set_favorite_shown(&mut self, path: &Path, favorite: bool, cx: &mut Context<Self>) {
        self.marks.set_favorite(path, favorite);
        if self.filter.is_favorites_only() {
            self.apply_filter(cx);
        }
        cx.notify();
    }

    fn render_rows(
        &mut self,
        rows: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let rem = window.rem_size();
        let columns = self.columns;
        let gap = rem * GAP;
        let tile = tiles::tile_size(self.width, columns, rem);
        let images = self.images.clone();
        rows.map(|row| {
            let start = row * columns;
            let start = start.min(images.len());
            let end = (start + columns).min(images.len());
            h_flex()
                .px(rem * PADDING)
                .pb(gap)
                .gap(gap)
                .children(
                    images[start..end]
                        .iter()
                        .enumerate()
                        .map(|(column, image)| {
                            self.render_tile(
                                image,
                                start + column,
                                tile.width,
                                tile.height,
                                rem,
                                cx,
                            )
                        }),
                )
                .into_any_element()
        })
        .collect()
    }

    fn render_tile(
        &self,
        image: &ImageFile,
        ix: usize,
        width: Pixels,
        height: Pixels,
        rem: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let path = image.path();
        let selected = self.selected.as_deref() == Some(path);
        let name: SharedString = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned()
            .into();
        let id = ElementId::Name(path.to_string_lossy().into_owned().into());

        let content = match self.thumbnails.get(path) {
            Some(Thumbnail::Ready(file)) => img(file.clone())
                .size_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            Some(Thumbnail::Failed) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::TriangleAlert).small())
                .child("Can't show this image")
                .into_any_element(),
            None => div().into_any_element(),
        };

        let picture = div()
            .size_full()
            .rounded(theme.radius)
            .overflow_hidden()
            .bg(theme.muted)
            .child(content)
            .when(self.marks.is_favorite(path), |picture| {
                picture.child(
                    div()
                        .absolute()
                        .top_1p5()
                        .right_1p5()
                        .p_1()
                        .rounded_full()
                        .bg(theme.background.opacity(0.8))
                        .text_color(theme.warning)
                        .child(Icon::new(IconName::StarFill).small()),
                )
            })
            .when(self.is_current_wallpaper(path), |picture| {
                picture.child(tiles::picture_badge("Current", cx))
            });

        div()
            .id(id)
            .test_support()
            .relative()
            .flex_none()
            .w(width)
            .h(height)
            .child(picture)
            .map(|tile| match (self.selection.contains(path), selected) {
                (true, _) => tile.child(tiles::selection_ring(rem, cx)),
                // The keyboard's position, after ctrl-click took it out.
                (false, true) => tile.child(tiles::current_ring(rem, cx)),
                (false, false) => tile,
            })
            .tooltip(move |window, cx| Tooltip::new(name.clone()).build(window, cx))
            .on_click(cx.listener(move |grid, event: &ClickEvent, window, cx| {
                window.focus(&grid.focus_handle, cx);
                let modifiers = event.modifiers();
                if modifiers.shift {
                    grid.select_range(ix, cx);
                } else if modifiers.secondary() {
                    grid.toggle_selected(ix, cx);
                } else {
                    grid.select(ix, cx);
                    if event.click_count() == 2 {
                        cx.emit(OpenImage);
                    }
                }
            }))
            .into_any_element()
    }
}

impl EventEmitter<OpenImage> for LibraryGrid {}

impl Render for LibraryGrid {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.columns = tiles::columns(self.width, window.rem_size());
        let columns = self.columns;
        let rows = self.images.len().div_ceil(columns);
        let grid = cx.entity();

        div()
            .id("library-grid")
            .track_focus(&self.focus_handle)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|grid, _: &SelectLeft, _, cx| grid.move_selection(-1, cx)))
            .on_action(cx.listener(|grid, _: &SelectRight, _, cx| grid.move_selection(1, cx)))
            .on_action(cx.listener(|grid, _: &SelectUp, _, cx| {
                grid.move_selection(-(grid.columns as isize), cx)
            }))
            .on_action(cx.listener(|grid, _: &SelectDown, _, cx| {
                grid.move_selection(grid.columns as isize, cx)
            }))
            .on_action(cx.listener(|grid, _: &SelectFirst, _, cx| grid.select(0, cx)))
            .on_action(cx.listener(|grid, _: &SelectLast, _, cx| {
                let last = grid.images.len().saturating_sub(1);
                grid.select(last, cx)
            }))
            .on_action(cx.listener(|grid, _: &DeleteSelected, window, cx| {
                grid.confirm_delete_selected(window, cx)
            }))
            .on_action(cx.listener(|grid, _: &SetWallpaper, window, cx| {
                grid.set_selected_as_wallpaper(window, cx)
            }))
            .on_action(
                cx.listener(|grid, _: &FocusTagFilter, window, cx| {
                    grid.focus_tag_filter(window, cx)
                }),
            )
            .on_action(cx.listener(|grid, _: &Dismiss, window, cx| grid.dismiss(window, cx)))
            .on_action(cx.listener(|grid, _: &ExtendLeft, _, cx| grid.extend_selection(-1, cx)))
            .on_action(cx.listener(|grid, _: &ExtendRight, _, cx| grid.extend_selection(1, cx)))
            .on_action(cx.listener(|grid, _: &ExtendUp, _, cx| {
                grid.extend_selection(-(grid.columns as isize), cx)
            }))
            .on_action(cx.listener(|grid, _: &ExtendDown, _, cx| {
                grid.extend_selection(grid.columns as isize, cx)
            }))
            .on_action(cx.listener(|grid, _: &SelectAll, _, cx| grid.select_all(cx)))
            .on_action(cx.listener(|grid, _: &OpenSelected, _, cx| {
                if grid.selected.is_some() {
                    cx.emit(OpenImage);
                }
            }))
            .on_action(
                cx.listener(|grid, _: &ToggleFavorite, window, cx| {
                    grid.toggle_favorite(window, cx)
                }),
            )
            .flex()
            .flex_col()
            .size_full()
            .child(self.render_toolbar(cx))
            .children(self.render_tag_filter(cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        // Measures the grid; the column count depends on its width.
                        canvas(
                            move |bounds, _, cx| {
                                grid.update(cx, |grid, cx| grid.set_width(bounds.size.width, cx))
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .map(|area| {
                        if self.images.is_empty() && !self.all.is_empty() {
                            return area.child(self.render_no_matches(cx));
                        }
                        area.child(
                            image_cache(self.image_cache.clone()).size_full().child(
                                uniform_list(
                                    "library-grid-rows",
                                    rows,
                                    cx.processor(|grid, rows, window, cx| {
                                        grid.render_rows(rows, window, cx)
                                    }),
                                )
                                .pt(window.rem_size() * PADDING)
                                .track_scroll(&self.scroll_handle)
                                .size_full(),
                            ),
                        )
                        .child(Scrollbar::vertical(&self.scroll_handle))
                    }),
            )
    }
}

const ORIENTATIONS: [(Option<Orientation>, &str); 4] = [
    (None, "All"),
    (Some(Orientation::Landscape), "Landscape"),
    (Some(Orientation::Portrait), "Portrait"),
    (Some(Orientation::Square), "Square"),
];

const ORDERS: [(ImageOrder, &str); 4] = [
    (ImageOrder::Added, "Recently Added"),
    (ImageOrder::Modified, "Recently Modified"),
    (ImageOrder::Name, "Name"),
    (ImageOrder::Resolution, "Resolution"),
];

impl LibraryGrid {
    /// Sorting and filtering, always visible above the grid so the current
    /// view is never a hidden state.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let orientation = self.filter.orientation();
        let order = self.order;
        let order_name = ORDERS
            .iter()
            .find(|(candidate, _)| *candidate == order)
            .map_or("", |(_, name)| *name);
        let grid = cx.entity();

        h_flex()
            .flex_none()
            .px(rems(PADDING))
            .py_2()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                ToggleGroup::new("orientation")
                    .segmented()
                    .outline()
                    .children(ORIENTATIONS.iter().map(|(candidate, name)| {
                        Toggle::new(*name)
                            .label(*name)
                            .checked(*candidate == orientation)
                    }))
                    .on_click(cx.listener(|grid, checks: &Vec<bool>, _, cx| {
                        // A pick-one control: the newly checked item wins, and
                        // clicking the checked item again leaves it checked.
                        let current = ORIENTATIONS
                            .iter()
                            .position(|(candidate, _)| *candidate == grid.filter.orientation());
                        let picked = checks
                            .iter()
                            .enumerate()
                            .find(|(ix, checked)| **checked && Some(*ix) != current)
                            .map(|(ix, _)| ix);
                        if let Some(ix) = picked {
                            let filter = grid.filter.clone().with_orientation(ORIENTATIONS[ix].0);
                            grid.set_filter(filter, cx);
                        }
                    })),
            )
            .child(
                Toggle::new("favorites-only")
                    .outline()
                    .label("Favorites")
                    .checked(self.filter.is_favorites_only())
                    .on_click(cx.listener(|grid, checked: &bool, _, cx| {
                        let filter = grid.filter.clone().with_favorites_only(*checked);
                        grid.set_filter(filter, cx);
                    })),
            )
            .child(
                Input::new(&self.tag_query)
                    .id("tag-filter")
                    .w_48()
                    .prefix(Icon::new(IconName::Search).small()),
            )
            .child(div().flex_1())
            // Several selected: say so, and offer what applies to all of them, so
            // batch deletion doesn't depend on knowing the Delete key.
            .when(self.selection.len() > 1, |toolbar| {
                toolbar
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{} selected",
                                format::grouped(self.selection.len())
                            )),
                    )
                    .child(
                        Button::new("move-selection-to-trash")
                            .danger()
                            .outline()
                            .small()
                            .label("Move to Trash…")
                            .tooltip("Delete")
                            .on_click(cx.listener(|grid, _, window, cx| {
                                grid.confirm_delete_selected(window, cx)
                            })),
                    )
                    .child(
                        Button::new("deselect")
                            .ghost()
                            .small()
                            .label("Deselect")
                            .tooltip("Esc")
                            .on_click(cx.listener(|grid, _, _, cx| grid.collapse_selection(cx))),
                    )
            })
            .when(!self.filter.is_empty(), |toolbar| {
                toolbar.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{} of {}",
                            format::grouped(self.images.len()),
                            format::count(self.all.len(), "image", "images")
                        )),
                )
            })
            .child(
                Button::new("sort")
                    .ghost()
                    .small()
                    .icon(IconName::SortDescending)
                    .label(order_name)
                    .tooltip("Sort by")
                    .dropdown_menu(move |menu, _, _| {
                        ORDERS.iter().fold(menu, |menu, (candidate, name)| {
                            let candidate = *candidate;
                            let grid = grid.clone();
                            menu.item(
                                PopupMenuItem::new(*name)
                                    .checked(candidate == order)
                                    .on_click(move |_, _, cx| {
                                        grid.update(cx, |grid, cx| grid.set_order(candidate, cx))
                                    }),
                            )
                        })
                    }),
            )
    }

    /// The tags the grid is filtered by, and suggestions while typing. Shown
    /// under the toolbar only when there is something to show, wrapping so no
    /// selected tag is ever hidden.
    fn render_tag_filter(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let selected = self.filter.tags().to_vec();
        let typed = self.tag_query.read(cx).value().trim().to_owned();
        if selected.is_empty() && typed.is_empty() {
            return None;
        }
        let suggestions = self.tag_suggestions(cx);
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        Some(
            h_flex()
                .flex_none()
                .flex_wrap()
                .px(rems(PADDING))
                .py_2()
                .gap_1()
                .border_b_1()
                .border_color(theme.border)
                .text_sm()
                .children(selected.into_iter().map(|tag| {
                    let removed = tag.clone();
                    Tag::secondary().small().child(
                        h_flex().gap_1().child(tag.clone()).child(
                            Button::new(ElementId::Name(format!("remove-filter-tag-{tag}").into()))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip(format!("Stop filtering by \u{201c}{tag}\u{201d}"))
                                .on_click(cx.listener(move |grid, _, _, cx| {
                                    grid.remove_filter_tag(&removed, cx)
                                })),
                        ),
                    )
                }))
                .when(!typed.is_empty() && suggestions.is_empty(), |row| {
                    row.child(
                        div()
                            .text_color(muted)
                            .child(format!("No tags match \u{201c}{typed}\u{201d}")),
                    )
                })
                .children(suggestions.into_iter().map(|count| {
                    let name = count.name().to_owned();
                    Button::new(ElementId::Name(format!("filter-tag-{name}").into()))
                        .outline()
                        .xsmall()
                        .label(format!("{name} \u{b7} {}", format::grouped(count.images())))
                        .on_click(cx.listener(move |grid, _, window, cx| {
                            grid.add_filter_tag(name.clone(), window, cx)
                        }))
                }))
                .into_any_element(),
        )
    }

    fn render_no_matches(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title: SharedString = match (
            self.filter.orientation(),
            self.filter.is_favorites_only(),
            self.filter.tags(),
        ) {
            (Some(Orientation::Landscape), false, []) => "No landscape images".into(),
            (Some(Orientation::Portrait), false, []) => "No portrait images".into(),
            (Some(Orientation::Square), false, []) => "No square images".into(),
            (None, true, []) => "No favorites yet".into(),
            (None, false, [tag]) => format!("No images tagged \u{201c}{tag}\u{201d}").into(),
            (None, false, [_, _, ..]) => "No images have all these tags".into(),
            _ => "No images match".into(),
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .child(title)
            .when(self.filter.is_favorites_only(), |empty| {
                empty.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Select an image and press F to mark it as a favorite."),
                )
            })
            .child(
                Button::new("show-all")
                    .outline()
                    .small()
                    .label("Show All Images")
                    .on_click(
                        cx.listener(|grid, _, _, cx| grid.set_filter(ImageFilter::default(), cx)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use backdrop_core::library::Library;
    use backdrop_core::paths::AppPaths;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, Entity, SharedString, TestAppContext, px, size};
    use image::{Rgb, RgbImage};

    use super::LibraryGrid;

    /// A trash that moves files into a folder of its own.
    struct FolderTrash(PathBuf);

    impl backdrop_core::library::Trash for FolderTrash {
        fn trash(&self, file: &std::path::Path) -> Result<(), String> {
            std::fs::rename(file, self.0.join(file.file_name().unwrap()))
                .map_err(|error| error.to_string())
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        paths: AppPaths,
        /// The fake trash folder deleted images are moved to.
        trash_dir: PathBuf,
        trash: std::sync::Arc<dyn backdrop_core::library::Trash>,
    }

    /// A scanned library of `count` small landscape images.
    fn library(count: u8) -> Fixture {
        library_of(&vec![(64, 40); count as usize])
    }

    /// A scanned library with one image of each `(width, height)`.
    fn library_of(sizes: &[(u32, u32)]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir(&root).unwrap();
        for (ix, (width, height)) in sizes.iter().enumerate() {
            RgbImage::from_pixel(*width, *height, Rgb([ix as u8 * 30, 80, 120]))
                .save(root.join(format!("{ix}.png")))
                .unwrap();
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
        let trash_dir = dir.path().join("trash");
        std::fs::create_dir(&trash_dir).unwrap();
        Fixture {
            _dir: dir,
            root,
            paths,
            trash: std::sync::Arc::new(FolderTrash(trash_dir.clone())),
            trash_dir,
        }
    }

    fn open_grid(
        fixture: &Fixture,
        cx: &mut TestAppContext,
    ) -> (gpui_kit::AnyWindowHandle, Entity<LibraryGrid>) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::super::bind_keys(cx);
        });
        let mut grid = None;
        let handle = cx.open_window(size(px(800.), px(600.)), |window, cx| {
            let view = cx.new(|cx| {
                LibraryGrid::new(
                    fixture.root.clone(),
                    fixture.paths.clone(),
                    fixture.trash.clone(),
                    window,
                    cx,
                )
            });
            grid = Some(view.clone());
            Root::new(view, window, cx)
        });
        cx.run_until_parked();
        (handle.into(), grid.unwrap())
    }

    fn tile_id(grid: &Entity<LibraryGrid>, ix: usize, cx: &mut TestAppContext) -> SharedString {
        cx.update(|cx| {
            grid.read(cx).images[ix]
                .path()
                .to_string_lossy()
                .into_owned()
                .into()
        })
    }

    fn selected_ix(grid: &Entity<LibraryGrid>, cx: &mut TestAppContext) -> Option<usize> {
        cx.update(|cx| grid.read(cx).selected_ix())
    }

    #[gpui_kit::test]
    fn selection_follows_pointer_and_arrow_keys(cx: &mut TestAppContext) {
        let fixture = library(7);
        let (window, grid) = open_grid(&fixture, cx);
        let first = tile_id(&grid, 0, cx);
        let third = tile_id(&grid, 2, cx);

        cx.update_window(window, |_, window, cx| {
            // The second frame lays out the columns measured in the first.
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(window.find(first.clone()).visible());
            window.click(third.clone(), cx);
        })
        .unwrap();
        // 800 px fits three columns of tiles at least 13 rem wide.
        assert_eq!(cx.update(|cx| grid.read(cx).columns), 3);
        assert_eq!(selected_ix(&grid, cx), Some(2));

        let press = |key: &str, cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, cx| window.press(key, cx))
                .unwrap();
            selected_ix(&grid, cx)
        };
        assert_eq!(press("left", cx), Some(1));
        assert_eq!(press("down", cx), Some(4));
        assert_eq!(press("down", cx), Some(6), "stops at the last image");
        assert_eq!(press("up", cx), Some(3));
        assert_eq!(press("home", cx), Some(0));
        assert_eq!(press("left", cx), Some(0), "stays at the first image");
        assert_eq!(press("end", cx), Some(6));
    }

    #[gpui_kit::test]
    fn f_toggles_the_selected_favorite(cx: &mut TestAppContext) {
        let fixture = library(3);
        let (window, grid) = open_grid(&fixture, cx);
        let id = tile_id(&grid, 1, cx);
        let path = PathBuf::from(id.as_str());

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            // Nothing selected: F does nothing.
            window.press("f", cx);
            window.click(id.clone(), cx);
            window.press("f", cx);
        })
        .unwrap();
        cx.run_until_parked();

        let favorites = |fixture: &Fixture| {
            Library::open(&fixture.root, &fixture.paths.database_file())
                .unwrap()
                .favorites()
                .unwrap()
        };
        assert!(cx.update(|cx| grid.read(cx).is_favorite(&path)));
        assert_eq!(favorites(&fixture).into_iter().collect::<Vec<_>>(), [path]);

        cx.update_window(window, |_, window, cx| window.press("f", cx))
            .unwrap();
        cx.run_until_parked();

        assert!(favorites(&fixture).is_empty());
    }

    #[gpui_kit::test]
    fn toolbar_filters_by_orientation_and_favorites(cx: &mut TestAppContext) {
        // Two landscape images and one portrait image.
        let fixture = library_of(&[(64, 40), (40, 64), (64, 40)]);
        let (window, grid) = open_grid(&fixture, cx);
        let shown = |cx: &mut TestAppContext| cx.update(|cx| grid.read(cx).images().len());
        let click = |id: &'static str, cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.click(id, cx);
            })
            .unwrap();
        };
        assert_eq!(shown(cx), 3);

        click("Portrait", cx);
        assert_eq!(shown(cx), 1);
        let portrait = cx.update(|cx| grid.read(cx).images()[0].clone());
        assert_eq!((portrait.width(), portrait.height()), (40, 64));

        // No square images: the empty state offers a way back.
        click("Square", cx);
        assert_eq!(shown(cx), 0);
        click("show-all", cx);
        assert_eq!(shown(cx), 3);

        click("favorites-only", cx);
        assert_eq!(shown(cx), 0);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("show-all").visible());
        })
        .unwrap();

        // Favoriting an image shows it under the Favorites filter.
        let path = portrait.path().to_path_buf();
        cx.update(|cx| {
            grid.update(cx, |grid, cx| grid.set_favorite_shown(&path, true, cx));
        });
        assert_eq!(shown(cx), 1);
        click("favorites-only", cx);
        assert_eq!(shown(cx), 3);
    }

    /// A desktop shell that keeps the wallpaper in memory.
    struct FakeDesktop(std::sync::Mutex<Option<PathBuf>>);

    impl backdrop_core::desktop::WallpaperBackend for FakeDesktop {
        fn name(&self) -> &'static str {
            "Fake"
        }

        fn current_wallpaper(
            &self,
        ) -> Result<Option<PathBuf>, backdrop_core::desktop::DesktopError> {
            Ok(self.0.lock().unwrap().clone())
        }

        fn set_wallpaper(
            &self,
            image: &std::path::Path,
        ) -> Result<(), backdrop_core::desktop::DesktopError> {
            *self.0.lock().unwrap() = Some(image.to_path_buf());
            Ok(())
        }
    }

    #[gpui_kit::test]
    fn w_sets_the_selected_image_as_wallpaper(cx: &mut TestAppContext) {
        let fixture = library(3);
        let (window, grid) = open_grid(&fixture, cx);
        let path = |ix: usize, cx: &mut TestAppContext| {
            cx.update(|cx| grid.read(cx).images()[ix].path().to_path_buf())
        };
        let (first, second) = (path(0, cx), path(1, cx));
        let desktop = std::sync::Arc::new(FakeDesktop(std::sync::Mutex::new(Some(
            fixture.root.join(&first),
        ))));
        let shared = desktop.clone();
        cx.update(|cx| grid.update(cx, |grid, cx| grid.set_desktop(Some(shared), cx)));
        cx.run_until_parked();
        let is_current = |path: &PathBuf, cx: &mut TestAppContext| {
            cx.update(|cx| grid.read(cx).is_current_wallpaper(path))
        };
        assert!(is_current(&first, cx));

        let id: SharedString = second.to_string_lossy().into_owned().into();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.click(id.clone(), cx);
            window.press("w", cx);
        })
        .unwrap();
        cx.run_until_parked();

        assert_eq!(*desktop.0.lock().unwrap(), Some(fixture.root.join(&second)));
        assert!(is_current(&second, cx));
        assert!(!is_current(&first, cx));

        // Pressing W on the current wallpaper changes nothing.
        *desktop.0.lock().unwrap() = None;
        cx.update_window(window, |_, window, cx| window.press("w", cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(*desktop.0.lock().unwrap(), None);
    }

    #[gpui_kit::test]
    fn delete_asks_then_moves_the_selected_image_to_the_trash(cx: &mut TestAppContext) {
        let fixture = library(3);
        let (window, grid) = open_grid(&fixture, cx);
        let second = cx.update(|cx| grid.read(cx).images()[1].path().to_path_buf());
        let id: SharedString = second.to_string_lossy().into_owned().into();
        let shown = |cx: &mut TestAppContext| cx.update(|cx| grid.read(cx).images().len());
        let press_delete_then = |button: &'static str, cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, cx| window.press("delete", cx))
                .unwrap();
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.click(button, cx);
            })
            .unwrap();
            cx.run_until_parked();
        };
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.click(id.clone(), cx);
        })
        .unwrap();

        press_delete_then("cancel", cx);
        assert!(fixture.root.join(&second).exists());
        assert_eq!(shown(cx), 3);

        press_delete_then("ok", cx);
        assert!(!fixture.root.join(&second).exists());
        assert!(fixture.trash_dir.join(second.file_name().unwrap()).exists());
        assert_eq!(shown(cx), 2);
        assert_eq!(cx.update(|cx| grid.read(cx).total()), Some(2));
        // The image that took its place is selected.
        assert_eq!(cx.update(|cx| grid.read(cx).selected_ix()), Some(1));
    }
    #[gpui_kit::test]
    fn the_tag_field_suggests_tags_and_narrows_the_grid_to_all_of_them(cx: &mut TestAppContext) {
        let fixture = library(3);
        {
            let mut library = Library::open(&fixture.root, &fixture.paths.database_file()).unwrap();
            let tags = |list: &[&str]| list.iter().map(|tag| (*tag).to_owned()).collect::<Vec<_>>();
            library
                .add_tags(std::path::Path::new("0.png"), &tags(&["Night", "lake"]))
                .unwrap();
            library
                .add_tags(std::path::Path::new("1.png"), &tags(&["night"]))
                .unwrap();
        }
        let (window, grid) = open_grid(&fixture, cx);
        let shown = |cx: &mut TestAppContext| -> Vec<String> {
            let mut paths: Vec<String> = cx.update(|cx| {
                grid.read(cx)
                    .images()
                    .iter()
                    .map(|image| image.path().display().to_string())
                    .collect()
            });
            paths.sort();
            paths
        };
        let act = |cx: &mut TestAppContext,
                   act: &dyn Fn(&mut gpui_kit::Window, &mut gpui_kit::App)| {
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                act(window, cx);
            })
            .unwrap();
            cx.run_until_parked();
        };
        let focus_grid = cx.update(|cx| grid.read(cx).focus_handle().clone());
        cx.update_window(window, |_, window, cx| window.focus(&focus_grid, cx))
            .unwrap();

        // Enter picks the top suggestion, in the user's spelling.
        act(cx, &|window, cx| {
            window.press("ctrl-f", cx);
            window.input("nig", cx);
        });
        act(cx, &|window, cx| window.press("enter", cx));
        assert_eq!(shown(cx), ["0.png", "1.png"]);
        assert_eq!(
            cx.update(|cx| grid.read(cx).filter().tags().to_vec()),
            ["Night"]
        );

        // A second tag narrows to images with both.
        act(cx, &|window, cx| window.input("LAK", cx));
        act(cx, &|window, cx| window.click("filter-tag-lake", cx));
        assert_eq!(shown(cx), ["0.png"]);

        // Escape returns to the images, so arrow keys select again.
        act(cx, &|window, cx| {
            window.press("escape", cx);
            window.press("right", cx);
        });
        assert_eq!(cx.update(|cx| grid.read(cx).selected_ix()), Some(0));

        act(cx, &|window, cx| {
            window.click("remove-filter-tag-Night", cx)
        });
        assert_eq!(shown(cx), ["0.png"]);
        act(cx, &|window, cx| window.click("remove-filter-tag-lake", cx));
        assert_eq!(shown(cx), ["0.png", "1.png", "2.png"]);
    }
    /// Clicks the element `id` while holding `modifiers`.
    fn click_with(
        window: &mut gpui_kit::Window,
        id: SharedString,
        modifiers: gpui_kit::Modifiers,
        cx: &mut gpui_kit::App,
    ) {
        use gpui_kit::{
            InputEvent as _, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
        };
        window.render_frame(cx);
        let position = window.find(id).bounds().center();
        let move_to = MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers,
        };
        window.dispatch_event(move_to.to_platform_input(), cx);
        let down = MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
            first_mouse: false,
        };
        window.dispatch_event(down.to_platform_input(), cx);
        let up = MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
        };
        window.dispatch_event(up.to_platform_input(), cx);
        window.render_frame(cx);
    }

    #[gpui_kit::test]
    fn modifier_clicks_shift_arrows_and_ctrl_a_build_a_selection(cx: &mut TestAppContext) {
        let fixture = library(6);
        let (window, grid) = open_grid(&fixture, cx);
        let ids: Vec<_> = (0..6).map(|ix| tile_id(&grid, ix, cx)).collect();
        let count = |cx: &mut TestAppContext| cx.update(|cx| grid.read(cx).selection_count());
        let ctrl = gpui_kit::Modifiers {
            control: true,
            ..Default::default()
        };
        let shift = gpui_kit::Modifiers {
            shift: true,
            ..Default::default()
        };
        let act = |cx: &mut TestAppContext,
                   act: &dyn Fn(&mut gpui_kit::Window, &mut gpui_kit::App)| {
            cx.update_window(window, |_, window, cx| act(window, cx))
                .unwrap();
        };

        act(cx, &|window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.click(ids[0].clone(), cx);
        });
        assert_eq!(count(cx), 1);

        act(cx, &|window, cx| {
            click_with(window, ids[2].clone(), ctrl, cx)
        });
        assert_eq!(count(cx), 2);
        assert_eq!(selected_ix(&grid, cx), Some(2));

        // Shift selects from the last ctrl-clicked image.
        act(cx, &|window, cx| {
            click_with(window, ids[4].clone(), shift, cx)
        });
        assert_eq!(count(cx), 3, "images 2 to 4");
        act(cx, &|window, cx| window.press("shift-left", cx));
        assert_eq!(count(cx), 2, "images 2 and 3");
        assert_eq!(selected_ix(&grid, cx), Some(3));

        act(cx, &|window, cx| window.press("ctrl-a", cx));
        assert_eq!(count(cx), 6);
        act(cx, &|window, cx| window.press("escape", cx));
        assert_eq!(count(cx), 1);
        assert_eq!(selected_ix(&grid, cx), Some(3), "the current image stays");

        // Ctrl-click can take out the current image; it stays current.
        act(cx, &|window, cx| {
            click_with(window, ids[3].clone(), ctrl, cx)
        });
        assert_eq!(count(cx), 0);
        assert_eq!(selected_ix(&grid, cx), Some(3));
        // Plain movement selects one image again.
        act(cx, &|window, cx| window.press("right", cx));
        assert_eq!(count(cx), 1);
        let current = cx.update(|cx| grid.read(cx).images()[4].path().to_path_buf());
        assert!(cx.update(|cx| grid.read(cx).is_in_selection(&current)));
    }

    #[gpui_kit::test]
    fn the_selection_is_moved_to_the_trash_together(cx: &mut TestAppContext) {
        let fixture = library(4);
        let (window, grid) = open_grid(&fixture, cx);
        let ids: Vec<_> = (0..4).map(|ix| tile_id(&grid, ix, cx)).collect();
        let doomed: Vec<_> = (0..3)
            .map(|ix| cx.update(|cx| grid.read(cx).images()[ix].path().to_path_buf()))
            .collect();
        let shift = gpui_kit::Modifiers {
            shift: true,
            ..Default::default()
        };
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.click(ids[0].clone(), cx);
            click_with(window, ids[2].clone(), shift, cx);
            window.click("move-selection-to-trash", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("ok", cx);
        })
        .unwrap();
        cx.run_until_parked();

        for path in &doomed {
            assert!(!fixture.root.join(path).exists());
            assert!(fixture.trash_dir.join(path.file_name().unwrap()).exists());
        }
        assert_eq!(cx.update(|cx| grid.read(cx).images().len()), 1);
        assert_eq!(selected_ix(&grid, cx), Some(0), "the image left is current");
        assert_eq!(cx.update(|cx| grid.read(cx).selection_count()), 1);
    }
}
