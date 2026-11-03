//! The settings page, opened over either page from the title bar (ctrl-,).
//!
//! There is a single page, so it is composed from `GroupBox`es directly.
//! GPUI Kit's `Settings` component would add a navigation sidebar and search
//! for that one page.

use std::path::PathBuf;
use std::sync::Arc;

use backdrop_core::providers::Provider;
use backdrop_core::settings::LibraryStatus;
use gpui_kit::component::button::Button;
use gpui_kit::component::group_box::GroupBox;
use gpui_kit::component::label::Label;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;

use super::api_key_form::{ApiKeyChanged, ApiKeyForm};
use super::appearance::AppearanceSettings;
use super::image_detail::Back;

pub const KEY_CONTEXT: &str = "Settings";

/// Line length that keeps a row's title and its controls easy to connect.
const PAGE_MAX_WIDTH: f32 = 40.;

/// The user asked to leave the settings.
pub struct CloseSettings;

/// The user asked to choose another library folder.
pub struct ChangeLibraryFolder;

/// The library folder as the workspace knows it.
#[derive(Clone, Debug, Default)]
pub struct LibraryFolder {
    /// `None` while the settings are read, or when they can't be.
    pub status: Option<LibraryStatus>,
    /// The settings file couldn't be read, so no folder can be saved.
    pub unreadable: bool,
    /// A folder prompt or settings save is in progress.
    pub choosing: bool,
}

pub struct SettingsPage {
    library: LibraryFolder,
    appearance: Entity<AppearanceSettings>,
    /// Whose API key the provider group sets; a fake one in tests.
    provider: Arc<dyn Provider>,
    settings_file: PathBuf,
    _api_key_subscription: Option<Subscription>,
    focus_handle: FocusHandle,
}

impl SettingsPage {
    pub fn new(
        library: LibraryFolder,
        appearance: Entity<AppearanceSettings>,
        provider: Arc<dyn Provider>,
        settings_file: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        appearance.update(cx, |appearance, cx| appearance.sync_selects(window, cx));
        Self {
            library,
            appearance,
            provider,
            settings_file,
            _api_key_subscription: None,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    pub fn set_library(&mut self, library: LibraryFolder, cx: &mut Context<Self>) {
        self.library = library;
        cx.notify();
    }

    /// Opens the dialog for the provider's API key, with its field focused.
    fn open_api_key_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let provider = self.provider.clone();
        let settings_file = self.settings_file.clone();
        let form = cx.new(|cx| ApiKeyForm::new(provider, settings_file, window, cx));
        // The row says whether a key is set, and the browser's NSFW toggle
        // depends on one.
        self._api_key_subscription = Some(cx.subscribe(&form, |_, _, _: &ApiKeyChanged, cx| {
            cx.emit(ApiKeyChanged);
            cx.notify();
        }));
        let title: SharedString = format!("{} API key", self.provider.name()).into();
        // The builder can run again when the dialog layer is rebuilt; focus the
        // field only the first time.
        let initial_focus = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if initial_focus.replace(false) {
                let focus_handle = form.read(cx).input().read(cx).focus_handle(cx);
                window.focus(&focus_handle, cx);
            }
            let save = form.clone();
            dialog
                .title(title.clone())
                .child(form.clone())
                // Enter saves; the dialog stays open until the key is saved.
                .on_ok(move |_, window, cx| {
                    save.update(cx, |form, cx| form.save(window, cx));
                    false
                })
        });
    }

    fn render_library(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let danger = cx.theme().danger;
        let (description, label) = match &self.library.status {
            None if self.library.unreadable => (
                div()
                    .text_color(danger)
                    .child("Couldn’t read the settings file, so the folder can’t be changed."),
                "Change Folder…",
            ),
            None => (div(), "Change Folder…"),
            Some(LibraryStatus::NotChosen) => (div().child("Not chosen"), "Choose Folder…"),
            Some(LibraryStatus::Missing(dir)) => (
                div()
                    .text_color(danger)
                    .child(format!("Not found: {}", dir.display())),
                "Choose Folder…",
            ),
            Some(LibraryStatus::Available(dir)) => {
                (div().child(dir.display().to_string()), "Change Folder…")
            }
        };
        let change = Button::new("change-library-folder")
            .outline()
            .label(label)
            .disabled(self.library.status.is_none())
            .loading(self.library.choosing)
            .on_click(cx.listener(|_, _, _, cx| cx.emit(ChangeLibraryFolder)));
        GroupBox::new()
            .id("library")
            .title("Library")
            .child(setting_row(
                "library-folder",
                "Folder",
                description,
                change,
                cx,
            ))
    }

    fn render_provider(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_key = self.provider.has_api_key();
        let change = Button::new("api-key")
            .outline()
            .label(if has_key {
                "Change Key…"
            } else {
                "Add Key…"
            })
            .on_click(cx.listener(|this, _, window, cx| this.open_api_key_dialog(window, cx)));
        GroupBox::new()
            .id("provider")
            .title(self.provider.name())
            .child(setting_row(
                "api-key-row",
                "API key",
                if has_key {
                    "Saved. NSFW wallpapers can be shown."
                } else {
                    "Needed to show NSFW wallpapers."
                },
                change,
                cx,
            ))
    }
}

impl EventEmitter<CloseSettings> for SettingsPage {}
impl EventEmitter<ChangeLibraryFolder> for SettingsPage {}
impl EventEmitter<ApiKeyChanged> for SettingsPage {}

impl Render for SettingsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("settings")
            .track_focus(&self.focus_handle)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|_, _: &Back, _, cx| cx.emit(CloseSettings)))
            .size_full()
            .child(
                v_flex()
                    .w_full()
                    .max_w(rems(PAGE_MAX_WIDTH))
                    .px_4()
                    .py_6()
                    .gap_6()
                    .child(self.render_library(cx))
                    .child(self.render_provider(cx))
                    .child(self.appearance.clone()),
            )
            .overflow_y_scrollbar()
    }
}

/// A setting laid out like GPUI Kit's `SettingItem`: title and description
/// on the left, controls on the right.
pub fn setting_row(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    description: impl IntoElement,
    controls: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    h_flex()
        .id(id)
        .w_full()
        .justify_between()
        .items_center()
        .gap_3()
        .child(
            v_flex()
                .flex_1()
                .max_w_3_5()
                .child(Label::new(title).text_sm())
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(description),
                ),
        )
        .child(controls)
}
