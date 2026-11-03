//! The settings page, opened over either page from the title bar (ctrl-,).
//!
//! There is a single page, so it is composed from `GroupBox`es directly.
//! GPUI Kit's `Settings` component would add a navigation sidebar and search
//! for that one page.

use backdrop_core::settings::LibraryStatus;
use gpui_kit::component::button::Button;
use gpui_kit::component::group_box::GroupBox;
use gpui_kit::component::label::Label;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, h_flex, v_flex};
use gpui_kit::*;

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
    /// A folder prompt or settings save is in progress.
    pub choosing: bool,
}

pub struct SettingsPage {
    library: LibraryFolder,
    appearance: Entity<AppearanceSettings>,
    focus_handle: FocusHandle,
}

impl SettingsPage {
    pub fn new(
        library: LibraryFolder,
        appearance: Entity<AppearanceSettings>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        appearance.update(cx, |appearance, cx| appearance.sync_selects(window, cx));
        Self {
            library,
            appearance,
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

    fn render_library(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let danger = cx.theme().danger;
        let (description, label) = match &self.library.status {
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
}

impl EventEmitter<CloseSettings> for SettingsPage {}
impl EventEmitter<ChangeLibraryFolder> for SettingsPage {}

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
