//! The full-window view of one image: a large preview beside its details.
//!
//! The grid owns the images, the selection, and favorites; this view shows the
//! grid's selected image and moves the selection when stepping through images,
//! so returning to the grid lands on the image that was last shown.
//!
//! When the image came from a provider, the details also show what the
//! provider said about it: its page, rating, upload date, and tags, read from
//! the snapshot saved in the library. An image only recognized by its file
//! name can be linked from here, which fetches that snapshot.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use backdrop_core::image::ImageFile;
use backdrop_core::library::{ImageSource, Library, TagCount};
use backdrop_core::metadata::{normalize_tag, same_tag};
use backdrop_core::providers::Provider;
use backdrop_core::snapshot::Snapshot;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::description_list::DescriptionList;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::link::Link;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::image_cache::LruImageCache;
use super::library_grid::{DeleteSelected, LibraryGrid, SetWallpaper, ToggleFavorite};
use crate::format;

pub const KEY_CONTEXT: &str = "ImageDetail";

actions!(image_detail, [Back, PreviousImage, NextImage, FocusTags]);

/// Suggestions shown under the tag field.
const TAG_SUGGESTIONS: usize = 6;

/// Emitted when the user leaves the view.
pub struct BackToGrid;

/// Decoded images kept in memory: the current image, its thumbnail shown while
/// it loads, and the previous image for quick back-and-forth.
const DECODED_IMAGES: usize = 4;
pub const SIDEBAR_WIDTH: f32 = 20.;

/// Where an image came from, as the library records it.
struct Provenance {
    source: Option<ImageSource>,
    /// The provider's snapshot, for a recorded source that has one.
    snapshot: Option<Snapshot>,
}

pub struct ImageDetail {
    grid: Entity<LibraryGrid>,
    /// Recognizes provider file names and fetches details when linking.
    provider: Arc<dyn Provider>,
    database: PathBuf,
    /// The provenance of the image it was read for. Keyed by path, so a
    /// result that arrives after moving on is never shown for another image.
    provenance: Option<(PathBuf, Provenance)>,
    /// The image whose provenance is being read.
    reading: Option<PathBuf>,
    /// Details are being fetched to link the shown image.
    linking: bool,
    /// Adds a tag to the shown image.
    tag_input: Entity<InputState>,
    _tag_input_subscription: Subscription,
    focus_handle: FocusHandle,
    image_cache: Entity<LruImageCache>,
    _grid_observation: Subscription,
    _provenance_task: Option<Task<()>>,
}

impl ImageDetail {
    pub fn new(
        grid: Entity<LibraryGrid>,
        provider: Arc<dyn Provider>,
        database: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tag_input = cx.new(|cx| InputState::new(window, cx).placeholder("Add tag…"));
        let tag_input_subscription = cx.subscribe_in(
            &tag_input,
            window,
            |this, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => this.add_typed_tag(window, cx),
                // Suggestions follow what is typed.
                InputEvent::Change => cx.notify(),
                _ => {}
            },
        );
        let mut detail = Self {
            _grid_observation: cx.observe(&grid, |this, _, cx| {
                this.read_provenance(cx);
                cx.notify();
            }),
            grid,
            provider,
            database,
            provenance: None,
            reading: None,
            linking: false,
            tag_input,
            _tag_input_subscription: tag_input_subscription,
            focus_handle: cx.focus_handle(),
            image_cache: LruImageCache::new(DECODED_IMAGES, cx),
            _provenance_task: None,
        };
        detail.read_provenance(cx);
        detail
    }

    /// Adds `tag` to the shown image, unless it has it already.
    fn add_tag(&mut self, tag: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.shown_path(cx) else {
            return;
        };
        let Some(tag) = normalize_tag(tag) else {
            return;
        };
        let mut tags = self.grid.read(cx).tags(&path).to_vec();
        if !tags.iter().any(|existing| same_tag(existing, &tag)) {
            tags.push(tag);
            self.grid
                .update(cx, |grid, cx| grid.set_tags(path, tags, window, cx));
        }
        self.tag_input
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    fn add_typed_tag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.tag_input.read(cx).value().to_string();
        self.add_tag(&typed, window, cx);
    }

    fn remove_tag(&mut self, tag: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.shown_path(cx) else {
            return;
        };
        let tags = self
            .grid
            .read(cx)
            .tags(&path)
            .iter()
            .filter(|existing| !same_tag(existing, tag))
            .cloned()
            .collect();
        self.grid
            .update(cx, |grid, cx| grid.set_tags(path, tags, window, cx));
    }

    /// Tags in the library that contain the typed text and the shown image
    /// doesn't have, most used first.
    fn tag_suggestions(&self, path: &Path, cx: &App) -> Vec<String> {
        let typed = self.tag_input.read(cx).value().trim().to_lowercase();
        if typed.is_empty() {
            return Vec::new();
        }
        // The grid's counts follow tag edits at once.
        let grid = self.grid.read(cx);
        let has = grid.tags(path);
        grid.tag_counts()
            .iter()
            .map(TagCount::name)
            .filter(|name| name.to_lowercase().contains(&typed))
            .filter(|name| !has.iter().any(|tag| same_tag(tag, name)))
            .take(TAG_SUGGESTIONS)
            .map(str::to_owned)
            .collect()
    }

    fn shown_path(&self, cx: &App) -> Option<PathBuf> {
        let grid = self.grid.read(cx);
        let ix = grid.selected_ix()?;
        Some(grid.images()[ix].path().to_path_buf())
    }

    /// Reads the shown image's source and snapshot, unless they are already
    /// read or being read.
    fn read_provenance(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.shown_path(cx) else {
            return;
        };
        let current = self.provenance.as_ref().map(|(read, _)| read);
        if current == Some(&path) || self.reading.as_ref() == Some(&path) {
            return;
        }
        self.reload_provenance(path, cx);
    }

    fn reload_provenance(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.reading = Some(path.clone());
        let root = self.grid.read(cx).root().to_path_buf();
        let database = self.database.clone();
        let provider = self.provider.clone();
        self._provenance_task = Some(cx.spawn(async move |this, cx| {
            let read_path = path.clone();
            let provenance = cx
                .background_spawn(async move {
                    let library = Library::open(&root, &database).ok()?;
                    let source = library.image_source(provider.as_ref(), &read_path).ok()?;
                    let snapshot = match &source {
                        Some(ImageSource::Recorded(source)) => {
                            Snapshot::load(&root, source).ok().flatten()
                        }
                        _ => None,
                    };
                    Some(Provenance { source, snapshot })
                })
                .await
                // Unreadable metadata shows no provenance; the scan reports it.
                .unwrap_or(Provenance {
                    source: None,
                    snapshot: None,
                });
            this.update(cx, |detail, cx| {
                if detail.reading.as_ref() == Some(&path) {
                    detail.reading = None;
                    detail.provenance = Some((path, provenance));
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// The provenance of the shown image, once read.
    fn provenance(&self, path: &Path) -> Option<&Provenance> {
        let (read, provenance) = self.provenance.as_ref()?;
        (read == path).then_some(provenance)
    }

    /// Fetches the details of the wallpaper the shown image's file name names,
    /// and records it as the image's source with its snapshot.
    fn link_shown_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.shown_path(cx) else {
            return;
        };
        let Some(ImageSource::FileName(source)) = self
            .provenance(&path)
            .and_then(|provenance| provenance.source.clone())
        else {
            return;
        };
        let prefix = format!("{}/", self.provider.id());
        let Some(id) = source.strip_prefix(&prefix).map(str::to_owned) else {
            return;
        };
        if self.linking {
            return;
        }
        self.linking = true;
        cx.notify();
        let root = self.grid.read(cx).root().to_path_buf();
        let database = self.database.clone();
        let provider = self.provider.clone();
        let name = provider.name();
        cx.spawn_in(window, async move |this, cx| {
            let linked_path = path.clone();
            let result = cx
                .background_spawn(async move {
                    let wallpaper = provider.wallpaper(&id)?;
                    Library::open(&root, &database)?.link_source(&linked_path, &wallpaper)?;
                    // The library may already have had a snapshot of this
                    // wallpaper, and filters see that snapshot's tags.
                    let snapshot = Snapshot::load(&root, &wallpaper.source())?;
                    let tags = snapshot.map_or_else(Vec::new, |snapshot| {
                        snapshot.tags().iter().map(|tag| tag.name.clone()).collect()
                    });
                    anyhow::Ok(tags)
                })
                .await;
            this.update_in(cx, |detail, window, cx| {
                detail.linking = false;
                match result {
                    Ok(tags) => {
                        detail.grid.update(cx, |grid, cx| {
                            grid.set_provider_tags(&path, tags, cx);
                        });
                        detail.reload_provenance(path, cx);
                    }
                    Err(error) => window.push_notification(
                        Notification::error(error.to_string())
                            .title(format!("Couldn’t get the details from {name}")),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    #[cfg(test)]
    pub fn tags(&self, cx: &App) -> Vec<String> {
        self.shown_path(cx)
            .and_then(|path| {
                self.provenance(&path)?
                    .snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.tags().iter().map(|tag| tag.name.clone()).collect())
            })
            .unwrap_or_default()
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
        let tags = self.render_tags(image.path(), cx);
        let source = self.render_source(image.path(), cx);
        let theme = cx.theme();
        let path = image.path();
        let grid = self.grid.read(cx);
        let can_set_wallpaper = grid.has_desktop();
        let is_current = grid.is_current_wallpaper(path);
        let setting_wallpaper = grid.is_setting_wallpaper();
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
            .id("image-details")
            // Long tag lists scroll with the rest of the details.
            .overflow_y_scroll()
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
                    .when(can_set_wallpaper, |actions| {
                        actions.child(
                            Button::new("set-wallpaper")
                                .primary()
                                .label(if is_current {
                                    "Current Wallpaper"
                                } else {
                                    "Set as Wallpaper"
                                })
                                .disabled(is_current)
                                .loading(setting_wallpaper)
                                .tooltip("Show this image on the desktop (W)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.grid.update(cx, |grid, cx| {
                                        grid.set_selected_as_wallpaper(window, cx)
                                    })
                                })),
                        )
                    })
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
                    )
                    .child(
                        Button::new("move-to-trash")
                            .danger()
                            .outline()
                            .label("Move to Trash…")
                            .tooltip("Delete")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.grid
                                    .update(cx, |grid, cx| grid.confirm_delete_current(window, cx))
                            })),
                    ),
            )
            .child(tags)
            .children(source)
    }
}

impl ImageDetail {
    /// The user's tags of the image, with a field to add more. Below the
    /// actions, so they stay in place whatever the number of tags.
    fn render_tags(&self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let suggestions = self.tag_suggestions(path, cx);
        let tags = self.grid.read(cx).tags(path).to_vec();
        let theme = cx.theme();
        v_flex()
            .gap_2()
            .pt_4()
            .border_t_1()
            .border_color(theme.border)
            .child(div().text_sm().font_semibold().child("Tags"))
            .map(|section| {
                if tags.is_empty() {
                    return section.child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("No tags yet. Press T to add one."),
                    );
                }
                section.child(
                    h_flex()
                        .flex_wrap()
                        .gap_1()
                        .children(tags.into_iter().map(|tag| {
                            let removed = tag.clone();
                            Tag::secondary().small().child(
                                h_flex().gap_1().child(tag.clone()).child(
                                    Button::new(ElementId::Name(
                                        format!("remove-tag-{tag}").into(),
                                    ))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Close)
                                    .tooltip(format!("Remove “{tag}”"))
                                    .on_click(cx.listener(
                                        move |this, _, window, cx| {
                                            this.remove_tag(&removed, window, cx)
                                        },
                                    )),
                                ),
                            )
                        })),
                )
            })
            .child(Input::new(&self.tag_input).id("tag-input").small())
            .when(!suggestions.is_empty(), |section| {
                section.child(
                    h_flex()
                        .flex_wrap()
                        .gap_1()
                        .children(suggestions.into_iter().map(|name| {
                            let added = name.clone();
                            Button::new(ElementId::Name(format!("suggest-tag-{name}").into()))
                                .outline()
                                .xsmall()
                                .label(name)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.add_tag(&added, window, cx)
                                }))
                        })),
                )
            })
            .into_any_element()
    }

    /// Where the image came from and what its provider said about it, below
    /// the actions because it supplements the image rather than acting on it.
    fn render_source(&self, path: &Path, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let provenance = self.provenance(path)?;
        let source = provenance.source.as_ref()?;
        let heading = div().text_sm().font_semibold();
        let section = v_flex()
            .gap_2()
            .pt_4()
            .border_t_1()
            .border_color(theme.border);

        let snapshot =
            match (source, &provenance.snapshot) {
                (ImageSource::Recorded(_), Some(snapshot)) => snapshot,
                (ImageSource::Recorded(source), None) => {
                    return Some(
                        section
                            .child(heading.child("Source"))
                            .child(div().text_sm().child(source.clone()))
                            .into_any_element(),
                    );
                }
                (ImageSource::FileName(source), _) => {
                    return Some(
                        section
                            .child(heading.child("Source"))
                            .child(div().text_sm().text_color(theme.muted_foreground).child(
                                format!(
                                    "Named like a download of {source}. Get its details to \
                                     see its tags and keep the link if it's renamed."
                                ),
                            ))
                            .child(
                                Button::new("get-details")
                                    .outline()
                                    .small()
                                    .label("Get Details")
                                    .loading(self.linking)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.link_shown_image(window, cx)
                                    })),
                            )
                            .into_any_element(),
                    );
                }
            };

        let page = snapshot.page_url();
        let page_label = page
            .strip_prefix("https://")
            .or_else(|| page.strip_prefix("http://"))
            .unwrap_or(page)
            .to_owned();
        let mut details = DescriptionList::horizontal().columns(1).small().item(
            "Page",
            Link::new("provider-page")
                .href(page.to_owned())
                .child(page_label)
                .into_any_element(),
            1,
        );
        if let Some(rating) = snapshot.rating() {
            details = details.item("Rating", format::content_rating(rating), 1);
        }
        if let Some(uploaded) = snapshot.uploaded() {
            // The provider's own date, without a time zone; the day is enough.
            let day = uploaded.to_string();
            let day = day.split(['T', ' ']).next().unwrap_or(&day).to_owned();
            details = details.item("Uploaded", day, 1);
        }
        let tags = snapshot.tags();
        Some(
            section
                .child(heading.child(self.source_name(snapshot)))
                .child(details)
                .when(!tags.is_empty(), |section| {
                    section.child(
                        h_flex().flex_wrap().gap_1().children(
                            tags.iter()
                                .map(|tag| Tag::secondary().small().child(tag.name.clone())),
                        ),
                    )
                })
                .into_any_element(),
        )
    }

    /// The provider's name for a heading, e.g. "Wallhaven".
    fn source_name(&self, snapshot: &Snapshot) -> SharedString {
        let source = snapshot.source();
        let provider = source.split('/').next().unwrap_or_default();
        if provider == self.provider.id() {
            self.provider.name().into()
        } else {
            provider.to_owned().into()
        }
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
            // Escape leaves the tag field first, and the view after.
            .on_action(cx.listener(|this, _: &Back, window, cx| {
                if this.tag_input.read(cx).focus_handle(cx).is_focused(window) {
                    window.focus(&this.focus_handle, cx);
                } else {
                    cx.emit(BackToGrid)
                }
            }))
            .on_action(cx.listener(|this, _: &FocusTags, window, cx| {
                let focus_handle = this.tag_input.read(cx).focus_handle(cx);
                window.focus(&focus_handle, cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousImage, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &NextImage, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &DeleteSelected, window, cx| {
                this.grid
                    .update(cx, |grid, cx| grid.confirm_delete_current(window, cx))
            }))
            .on_action(cx.listener(|this, _: &SetWallpaper, window, cx| {
                this.grid
                    .update(cx, |grid, cx| grid.set_selected_as_wallpaper(window, cx))
            }))
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
