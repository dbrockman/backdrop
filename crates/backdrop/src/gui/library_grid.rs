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

use backdrop_core::image::ImageFile;
use backdrop_core::image::Orientation;
use backdrop_core::library::Library;
use backdrop_core::paths::AppPaths;
use backdrop_core::query::{ImageFilter, ImageOrder};
use backdrop_core::thumbnails::Thumbnails;
use gpui_kit::component::button::{
    Button, ButtonVariants as _, Toggle, ToggleGroup, ToggleVariants as _,
};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::format;
use super::image_cache::LruImageCache;

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
        ToggleFavorite
    ]
);

/// Emitted when the user asks to see the selected image in full.
pub struct OpenImage;

/// Decoded thumbnails kept in memory; well above the tiles visible at once.
const DECODED_THUMBNAILS: usize = 300;

/// Tile geometry, in rems so it follows the UI scale.
const MIN_TILE_WIDTH: f32 = 13.;
const GAP: f32 = 0.75;
const PADDING: f32 = 1.;
/// Tiles are 16:10, the most common wallpaper shape; others are cropped.
const TILE_ASPECT: f32 = 10. / 16.;
/// How far the selection ring sits outside the tile. Must stay below the gap
/// and the padding so the ring is never clipped.
const SELECTION_OFFSET: f32 = 0.25;

enum Thumbnail {
    Ready(Arc<Path>),
    Failed,
}

pub struct LibraryGrid {
    root: PathBuf,
    paths: AppPaths,
    /// Every image in the library, sorted by `order`.
    all: Vec<ImageFile>,
    /// The images shown: `all` narrowed by `filter`.
    images: Arc<Vec<ImageFile>>,
    order: ImageOrder,
    filter: ImageFilter,
    favorites: HashSet<PathBuf>,
    /// Keyed by image path; images without an entry are still being prepared.
    thumbnails: HashMap<PathBuf, Thumbnail>,
    /// The selected image, by path so it survives reordering.
    selected: Option<PathBuf>,
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
}

impl LibraryGrid {
    pub fn new(root: PathBuf, paths: AppPaths, cx: &mut Context<Self>) -> Self {
        let load_task = Self::load(&root, &paths, ImageOrder::default(), cx);
        Self {
            root,
            paths,
            all: Vec::new(),
            images: Arc::new(Vec::new()),
            order: ImageOrder::default(),
            filter: ImageFilter::default(),
            favorites: HashSet::new(),
            thumbnails: HashMap::new(),
            selected: None,
            width: px(0.),
            columns: 1,
            scroll_handle: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            image_cache: LruImageCache::new(DECODED_THUMBNAILS, cx),
            _load_task: load_task,
            _thumbnail_task: None,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    /// Reads the images and favorites from the index, which the caller has
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
                    anyhow::Ok((library.images(order)?, library.favorites()?))
                })
                .await;
            // A failed load leaves the grid as it was; the scan that preceded
            // it has already reported problems with the index.
            let Ok((images, favorites)) = loaded else {
                return;
            };
            this.update(cx, |grid, cx| {
                grid.all = images;
                grid.favorites = favorites;
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

    /// Recomputes the shown images. The selection is kept by path; while its
    /// image is filtered out, nothing is selected in the grid.
    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let images = self
            .all
            .iter()
            .filter(|image| self.filter.matches(image, &self.favorites))
            .cloned()
            .collect();
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

    fn columns_for_width(&self, window: &Window) -> usize {
        let rem = window.rem_size();
        let available = self.width - rem * (2. * PADDING) + rem * GAP;
        ((available / (rem * (MIN_TILE_WIDTH + GAP))).floor() as usize).max(1)
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

    /// The images in display order.
    pub fn images(&self) -> &Arc<Vec<ImageFile>> {
        &self.images
    }

    pub fn is_favorite(&self, path: &Path) -> bool {
        self.favorites.contains(path)
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

    /// Selects the image at `ix` and scrolls it into view.
    pub fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(image) = self.images.get(ix) else {
            return;
        };
        self.selected = Some(image.path().to_path_buf());
        self.scroll_handle
            .scroll_to_item(ix / self.columns, ScrollStrategy::Nearest);
        cx.notify();
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
        let favorite = !self.favorites.contains(&path);
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

    fn set_favorite_shown(&mut self, path: &Path, favorite: bool, cx: &mut Context<Self>) {
        if favorite {
            self.favorites.insert(path.to_path_buf());
        } else {
            self.favorites.remove(path);
        }
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
        let tile_width = ((self.width - rem * (2. * PADDING) - gap * (columns as f32 - 1.))
            / columns as f32)
            .max(px(1.));
        let tile_height = tile_width * TILE_ASPECT;
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
                                tile_width,
                                tile_height,
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
            .when(self.favorites.contains(path), |picture| {
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
            });

        div()
            .id(id)
            .test_support()
            .relative()
            .flex_none()
            .w(width)
            .h(height)
            .child(picture)
            // The selection ring sits outside the image, in the gap between
            // tiles, so it never covers the picture. Its radius grows by the
            // same offset so both corners stay concentric.
            .when(selected, |tile| {
                let offset = rem * SELECTION_OFFSET;
                tile.child(
                    div()
                        .absolute()
                        .top(-offset)
                        .left(-offset)
                        .right(-offset)
                        .bottom(-offset)
                        .rounded(theme.radius + offset)
                        .border_2()
                        .border_color(theme.primary),
                )
            })
            .tooltip(move |window, cx| Tooltip::new(name.clone()).build(window, cx))
            .on_click(cx.listener(move |grid, event: &ClickEvent, window, cx| {
                window.focus(&grid.focus_handle, cx);
                grid.select(ix, cx);
                if event.click_count() == 2 {
                    cx.emit(OpenImage);
                }
            }))
            .into_any_element()
    }
}

impl EventEmitter<OpenImage> for LibraryGrid {}

impl Render for LibraryGrid {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.columns = self.columns_for_width(window);
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
            .child(div().flex_1())
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

    fn render_no_matches(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match (self.filter.orientation(), self.filter.is_favorites_only()) {
            (Some(Orientation::Landscape), false) => "No landscape images",
            (Some(Orientation::Portrait), false) => "No portrait images",
            (Some(Orientation::Square), false) => "No square images",
            (None, true) => "No favorites yet",
            _ => "No images match",
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

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        paths: AppPaths,
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
        Fixture {
            _dir: dir,
            root,
            paths,
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
            let view =
                cx.new(|cx| LibraryGrid::new(fixture.root.clone(), fixture.paths.clone(), cx));
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
        assert!(cx.update(|cx| grid.read(cx).favorites.contains(&path)));
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
}
