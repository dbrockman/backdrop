//! The full-window view of one Wallhaven result: the full image beside its
//! details and tags, with a way to download it.
//!
//! As with the library's detail view, the browser owns the results, the
//! selection, and downloads; this view shows the browser's selected wallpaper
//! and moves its selection, so going back lands on the wallpaper shown last.
//!
//! Search results lack tags and the uploader, and their thumbnails are small,
//! so this view fetches the wallpaper's details and its full image. Both wait
//! until the selection has rested briefly, so stepping quickly through results
//! neither spends the API's rate limit nor downloads images nobody looks at,
//! and a full image still downloading when the user moves on is cancelled.
//!
//! Clicking a tag returns to the results, searching for exactly that tag.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use backdrop_core::providers::{Provider, RemoteWallpaper, Tag};
use backdrop_core::remote_images::{RemoteImageError, RemotePreviews};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::description_list::DescriptionList;
use gpui_kit::component::link::Link;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::image_cache::LruImageCache;
use super::image_detail::{Back, NextImage, PreviousImage, SIDEBAR_WIDTH};
use super::wallhaven_browser::{DownloadProgress, DownloadSelected, WallhavenBrowser};
use crate::format;

pub const KEY_CONTEXT: &str = "WallhavenDetail";

/// Emitted when the user leaves the view.
pub struct BackToResults;

/// How long the selection must rest before details and the full image load.
const SETTLE: Duration = Duration::from_millis(250);
/// Decoded images kept in memory: the shown image and the one before it.
const DECODED_IMAGES: usize = 2;

/// A wallpaper's full details, with tags.
enum Details {
    Loading,
    Loaded(Box<RemoteWallpaper>),
    Failed(SharedString),
}

/// The full image of the shown wallpaper.
enum Preview {
    Loading(Arc<DownloadProgress>),
    Ready(Arc<Path>),
    Failed(SharedString),
}

pub struct WallhavenDetail {
    browser: Entity<WallhavenBrowser>,
    provider: Arc<dyn Provider>,
    previews: RemotePreviews,
    /// Details fetched this session, by wallpaper id.
    details: HashMap<String, Details>,
    /// The id of the wallpaper shown, and its full image.
    preview: Option<(String, Preview)>,
    /// Stops the full image download of the wallpaper shown before.
    cancel: Arc<AtomicBool>,
    focus_handle: FocusHandle,
    image_cache: Entity<LruImageCache>,
    _browser_observation: Subscription,
    _load_task: Option<Task<()>>,
}

impl WallhavenDetail {
    pub fn new(
        browser: Entity<WallhavenBrowser>,
        provider: Arc<dyn Provider>,
        previews: RemotePreviews,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut detail = Self {
            _browser_observation: cx.observe(&browser, |this, _, cx| {
                this.selection_changed(cx);
                cx.notify();
            }),
            browser,
            provider,
            previews,
            details: HashMap::new(),
            preview: None,
            cancel: Arc::new(AtomicBool::new(false)),
            focus_handle: cx.focus_handle(),
            image_cache: LruImageCache::new(DECODED_IMAGES, cx),
            _load_task: None,
        };
        detail.selection_changed(cx);
        detail
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    /// Shows the results for `tag`, leaving this view.
    fn search_tag(&mut self, tag: Tag, window: &mut Window, cx: &mut Context<Self>) {
        self.browser
            .update(cx, |browser, cx| browser.search_tag(tag, window, cx));
        cx.emit(BackToResults);
    }

    fn shown(&self, cx: &App) -> Option<RemoteWallpaper> {
        self.browser.read(cx).selected_wallpaper().cloned()
    }

    /// Starts loading the details and full image of a newly shown wallpaper,
    /// once the selection has rested.
    fn selection_changed(&mut self, cx: &mut Context<Self>) {
        let Some(wallpaper) = self.shown(cx) else {
            return;
        };
        let id = wallpaper.id().to_owned();
        if self.preview.as_ref().is_some_and(|(shown, _)| *shown == id) {
            return;
        }
        self.cancel.store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        let progress = Arc::new(DownloadProgress::default());
        progress.total.store(u64::MAX, Ordering::Relaxed);
        self.preview = Some((id.clone(), Preview::Loading(progress.clone())));
        let needs_details = !matches!(self.details.get(&id), Some(Details::Loaded(_)));
        if needs_details {
            self.details.insert(id.clone(), Details::Loading);
        }

        let provider = self.provider.clone();
        let previews = self.previews.clone();
        self._load_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTLE).await;
            let details = needs_details.then(|| {
                let provider = provider.clone();
                let id = id.clone();
                cx.background_spawn(async move { provider.wallpaper(&id) })
            });
            let preview = cx.background_spawn(async move {
                previews.ensure(
                    provider.as_ref(),
                    &wallpaper,
                    &mut |received, total| {
                        progress.received.store(received, Ordering::Relaxed);
                        progress
                            .total
                            .store(total.unwrap_or(u64::MAX), Ordering::Relaxed);
                    },
                    &cancel,
                )
            });
            // Redraw while the full image arrives, so its progress moves.
            let redraw = {
                let this = this.clone();
                cx.spawn(async move |cx| {
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(100))
                            .await;
                        if this.update(cx, |_, cx| cx.notify()).is_err() {
                            break;
                        }
                    }
                })
            };
            if let Some(details) = details {
                let result = details.await;
                let id = id.clone();
                this.update(cx, |detail, cx| {
                    detail.details.insert(
                        id,
                        match result {
                            Ok(wallpaper) => Details::Loaded(Box::new(wallpaper)),
                            Err(error) => Details::Failed(error.to_string().into()),
                        },
                    );
                    cx.notify();
                })
                .ok();
            }
            let result = preview.await;
            drop(redraw);
            this.update(cx, |detail, cx| {
                if !detail
                    .preview
                    .as_ref()
                    .is_some_and(|(shown, _)| *shown == id)
                {
                    return;
                }
                let preview = match result {
                    Ok(file) => Preview::Ready(file.into()),
                    // Only happens after moving on, which replaced the preview.
                    Err(RemoteImageError::Cancelled) => return,
                    Err(error) => Preview::Failed(error.to_string().into()),
                };
                detail.preview = Some((id, preview));
                cx.notify();
            })
            .ok();
        }));
    }

    /// The tags of the shown wallpaper, once its details are loaded.
    #[cfg(test)]
    pub fn tags(&self, cx: &App) -> Vec<String> {
        let Some(wallpaper) = self.shown(cx) else {
            return Vec::new();
        };
        match self.details.get(wallpaper.id()) {
            Some(Details::Loaded(details)) => details
                .tags()
                .unwrap_or_default()
                .iter()
                .map(|tag| tag.name.clone())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The cached full image of the shown wallpaper, once it is loaded.
    #[cfg(test)]
    pub fn preview_file(&self) -> Option<Arc<Path>> {
        match self.preview.as_ref()? {
            (_, Preview::Ready(file)) => Some(file.clone()),
            _ => None,
        }
    }

    /// Downloads the shown wallpaper, from its full image when that has
    /// loaded, so it isn't downloaded twice.
    fn download(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cached = match (self.shown(cx), &self.preview) {
            (Some(shown), Some((id, Preview::Ready(file)))) if shown.id() == id => {
                Some(file.to_path_buf())
            }
            _ => None,
        };
        self.browser.update(cx, |browser, cx| {
            browser.download_selected_from(cached, window, cx)
        });
    }

    fn step(&mut self, offset: isize, cx: &mut Context<Self>) {
        self.browser
            .update(cx, |browser, cx| browser.move_selection(offset, cx));
    }

    fn render_preview(&self, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let content = match self.preview.as_ref().map(|(_, preview)| preview) {
            Some(Preview::Ready(file)) => img(file.clone())
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            Some(Preview::Failed(message)) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(theme.muted_foreground)
                .child("Couldn’t load the image")
                .child(div().text_sm().child(message.clone()))
                .into_any_element(),
            Some(Preview::Loading(progress)) => {
                let percent = progress.percent();
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .text_color(theme.muted_foreground)
                    .child(h_flex().gap_2().child(Spinner::new()).child(match percent {
                        Some(percent) => {
                            format!("Loading the full image… {}%", percent.floor())
                        }
                        None => "Loading the full image…".to_owned(),
                    }))
                    .child(
                        div().w_64().child(
                            Progress::new("preview-progress")
                                .loading(percent.is_none())
                                .value(percent.unwrap_or(0.))
                                .accessibility_label("Loading the full image"),
                        ),
                    )
                    .into_any_element()
            }
            None => div().into_any_element(),
        };
        image_cache(self.image_cache.clone())
            .flex_1()
            .min_w_0()
            .h_full()
            .p_6()
            .child(content)
            .into_any_element()
    }

    fn render_details(&self, shown: &RemoteWallpaper, cx: &mut Context<Self>) -> AnyElement {
        let details = self.details.get(shown.id());
        // The full details once loaded; the search result until then.
        let wallpaper = match details {
            Some(Details::Loaded(details)) => details.as_ref(),
            _ => shown,
        };
        let browser = self.browser.read(cx);
        let id = wallpaper.id().to_owned();
        let download = if browser.is_in_library(&id) {
            Button::new("download")
                .primary()
                .label("In Library")
                .disabled(true)
        } else if let Some(percent) = browser.download_progress(&id) {
            Button::new("download")
                .primary()
                .label(match percent {
                    Some(percent) => format!("Downloading… {}%", percent.floor()),
                    None => "Downloading…".to_owned(),
                })
                .loading(true)
        } else {
            let available = browser.has_library();
            Button::new("download")
                .primary()
                .label("Download")
                .disabled(!available)
                .tooltip(if available {
                    "Download into the library (D)"
                } else {
                    "Choose a library folder to download into"
                })
                .on_click(cx.listener(|this, _, window, cx| this.download(window, cx)))
        };

        let theme = cx.theme();
        let (width, height) = (wallpaper.width(), wallpaper.height());
        let mut list = DescriptionList::horizontal()
            .columns(1)
            .small()
            .item("Dimensions", format::dimensions(width, height), 1)
            .item("Aspect ratio", format::aspect_ratio(width, height), 1);
        if let Some(size) = wallpaper.file_size() {
            list = list.item("File size", format::file_size(size), 1);
        }
        list = list.item("Rating", format::content_rating(wallpaper.rating()), 1);
        if let Some(category) = wallpaper.category() {
            list = list.item("Category", category.to_owned(), 1);
        }
        if let Some(uploaded) = wallpaper.uploaded() {
            let day = uploaded.to_string();
            let day = day.split(['T', ' ']).next().unwrap_or(&day).to_owned();
            list = list.item("Uploaded", day, 1);
        }
        if let Some(uploader) = wallpaper.uploader() {
            list = list.item("Uploader", uploader.to_owned(), 1);
        }
        if let Some(views) = wallpaper.views() {
            list = list.item("Views", format::grouped(views as usize), 1);
        }
        if let Some(favorites) = wallpaper.favorites() {
            list = list.item("Favorites", format::grouped(favorites as usize), 1);
        }
        list = list.item(
            "Page",
            Link::new("wallpaper-page")
                .href(wallpaper.page_url().to_owned())
                .child(format!("{} on wallhaven.cc", wallpaper.id()))
                .into_any_element(),
            1,
        );
        if let Some(source) = wallpaper.source_url() {
            list = list.item(
                "Source",
                Link::new("wallpaper-source")
                    .href(source.to_owned())
                    .child(source.to_owned())
                    .into_any_element(),
                1,
            );
        }

        let tags = match details {
            Some(Details::Loaded(details)) => {
                let tags = details.tags().unwrap_or_default();
                if tags.is_empty() {
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("No tags")
                        .into_any_element()
                } else {
                    h_flex()
                        .flex_wrap()
                        .gap_1()
                        .children(tags.iter().map(|tag| {
                            let searched = tag.clone();
                            Button::new(ElementId::Name(format!("search-tag-{}", tag.name).into()))
                                .outline()
                                .xsmall()
                                .label(tag.name.clone())
                                .tooltip(format!("Search for “{}”", tag.name))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.search_tag(searched.clone(), window, cx)
                                }))
                        }))
                        .into_any_element()
                }
            }
            Some(Details::Failed(message)) => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(format!("Couldn’t load the tags: {message}"))
                .into_any_element(),
            Some(Details::Loading) | None => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Loading tags…")
                .into_any_element(),
        };

        v_flex()
            .id("wallpaper-details")
            // Long tag lists scroll with the rest of the details.
            .overflow_y_scroll()
            .w(rems(SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .p_4()
            .gap_4()
            .border_l_1()
            .border_color(theme.border)
            .child(div().font_semibold().child(wallpaper.id().to_owned()))
            .child(list)
            .child(download)
            .child(
                v_flex()
                    .gap_2()
                    .pt_4()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(div().text_sm().font_semibold().child("Tags"))
                    .child(tags),
            )
            .into_any_element()
    }
}

impl EventEmitter<BackToResults> for WallhavenDetail {}

impl Render for WallhavenDetail {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = self.shown(cx);
        h_flex()
            .id("wallhaven-detail")
            .track_focus(&self.focus_handle)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|_, _: &Back, _, cx| cx.emit(BackToResults)))
            .on_action(cx.listener(|this, _: &PreviousImage, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &NextImage, _, cx| this.step(1, cx)))
            .on_action(
                cx.listener(|this, _: &DownloadSelected, window, cx| this.download(window, cx)),
            )
            .size_full()
            .items_stretch()
            .map(|view| match shown {
                Some(shown) => view
                    .child(self.render_preview(cx))
                    .child(self.render_details(&shown, cx)),
                // The results changed under the view, for example a new search.
                None => view.child(
                    v_flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .text_color(cx.theme().muted_foreground)
                        .child("This wallpaper is no longer in the results"),
                ),
            })
    }
}
