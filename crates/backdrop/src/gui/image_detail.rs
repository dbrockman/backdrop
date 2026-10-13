//! The full-window view of one image: a large preview beside its details.
//!
//! The grid owns the images, the selection, and favorites; this view shows the
//! grid's selected image and moves the selection when stepping through images,
//! so returning to the grid lands on the image that was last shown.

use std::path::Path;

use backdrop_core::image::ImageFile;
use gpui_kit::component::button::Button;
use gpui_kit::component::description_list::DescriptionList;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::format;
use super::image_cache::LruImageCache;
use super::library_grid::{LibraryGrid, ToggleFavorite};

pub const KEY_CONTEXT: &str = "ImageDetail";

actions!(image_detail, [Back, PreviousImage, NextImage]);

/// Emitted when the user leaves the view.
pub struct BackToGrid;

/// Decoded images kept in memory: the current image, its thumbnail shown while
/// it loads, and the previous image for quick back-and-forth.
const DECODED_IMAGES: usize = 4;
const SIDEBAR_WIDTH: f32 = 20.;

pub struct ImageDetail {
    grid: Entity<LibraryGrid>,
    focus_handle: FocusHandle,
    image_cache: Entity<LruImageCache>,
    _grid_observation: Subscription,
}

impl ImageDetail {
    pub fn new(grid: Entity<LibraryGrid>, cx: &mut Context<Self>) -> Self {
        Self {
            _grid_observation: cx.observe(&grid, |_, _, cx| cx.notify()),
            grid,
            focus_handle: cx.focus_handle(),
            image_cache: LruImageCache::new(DECODED_IMAGES, cx),
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    /// Shows the image `offset` places away, staying within the library.
    fn step(&mut self, offset: isize, cx: &mut Context<Self>) {
        self.grid.update(cx, |grid, cx| {
            let last = grid.images().len().saturating_sub(1);
            if let Some(ix) = grid.selected_ix() {
                grid.select(ix.saturating_add_signed(offset).min(last), cx);
            }
        });
    }

    fn render_preview(
        &self,
        file: &Path,
        thumbnail: Option<std::sync::Arc<Path>>,
        cx: &App,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        image_cache(self.image_cache.clone())
            .flex_1()
            .min_w_0()
            .h_full()
            .p_6()
            .child(
                img(file.to_path_buf())
                    .size_full()
                    .object_fit(ObjectFit::Contain)
                    // The thumbnail is already on disk and decodes at once.
                    .with_loading(move || match &thumbnail {
                        Some(thumbnail) => img(thumbnail.clone())
                            .size_full()
                            .object_fit(ObjectFit::Contain)
                            .into_any_element(),
                        None => div().into_any_element(),
                    })
                    .with_fallback(move || {
                        v_flex()
                            .size_full()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .text_color(muted)
                            .child(Icon::new(IconName::TriangleAlert))
                            .child("Can't show this image")
                            .into_any_element()
                    }),
            )
    }

    fn render_details(
        &self,
        root: &Path,
        image: &ImageFile,
        favorite: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let path = image.path();
        let file = root.join(path);
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned();
        let folder = match path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            Some(parent) => parent.display().to_string(),
            None => root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };

        v_flex()
            .w(rems(SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .p_4()
            .gap_4()
            .border_l_1()
            .border_color(theme.border)
            .child(div().font_semibold().child(name))
            .child(
                DescriptionList::horizontal()
                    .columns(1)
                    .small()
                    .item(
                        "Dimensions",
                        format::dimensions(image.width(), image.height()),
                        1,
                    )
                    .item(
                        "Aspect ratio",
                        format::aspect_ratio(image.width(), image.height()),
                        1,
                    )
                    .item("Format", format::image_format(image.format()), 1)
                    .item("File size", format::file_size(image.size()), 1)
                    .item("Added", format::date_time(image.added()), 1)
                    .item("Modified", format::date_time(image.modified()), 1)
                    .item("Folder", folder, 1),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        Button::new("favorite")
                            .outline()
                            .icon(if favorite {
                                IconName::StarFill
                            } else {
                                IconName::Star
                            })
                            .label("Favorite")
                            .selected(favorite)
                            .tooltip("Mark or unmark as a favorite (F)")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.grid
                                    .update(cx, |grid, cx| grid.toggle_favorite(window, cx))
                            })),
                    )
                    .child({
                        let file = file.clone();
                        Button::new("open")
                            .outline()
                            .icon(IconName::ExternalLink)
                            .label("Open")
                            .tooltip("Open in the default image viewer")
                            .on_click(move |_, _, cx| cx.open_with_system(&file))
                    })
                    .child(
                        Button::new("show-in-folder")
                            .outline()
                            .label("Show in Folder")
                            .on_click(move |_, _, cx| cx.reveal_path(&file)),
                    ),
            )
    }
}

impl EventEmitter<BackToGrid> for ImageDetail {}

impl Render for ImageDetail {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let grid = self.grid.read(cx);
        let current = grid.selected_ix().map(|ix| {
            let image = grid.images()[ix].clone();
            let favorite = grid.is_favorite(image.path());
            let thumbnail = grid.thumbnail(image.path());
            (image, favorite, thumbnail)
        });
        let root = grid.root().to_path_buf();

        h_flex()
            .id("image-detail")
            .track_focus(&self.focus_handle)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|_, _: &Back, _, cx| cx.emit(BackToGrid)))
            .on_action(cx.listener(|this, _: &PreviousImage, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &NextImage, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &ToggleFavorite, window, cx| {
                this.grid
                    .update(cx, |grid, cx| grid.toggle_favorite(window, cx))
            }))
            .size_full()
            .items_stretch()
            .map(|view| match current {
                Some((image, favorite, thumbnail)) => view
                    .child(self.render_preview(&root.join(image.path()), thumbnail, cx))
                    .child(self.render_details(&root, &image, favorite, cx)),
                // The image left the library during a rescan.
                None => view.child(
                    v_flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .text_color(cx.theme().muted_foreground)
                        .child("This image is no longer in the library"),
                ),
            })
    }
}
