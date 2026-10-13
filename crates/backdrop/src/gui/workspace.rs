use gpui_kit::component::empty::{Empty, EmptyDescription, EmptyHeader, EmptyTitle};
use gpui_kit::component::{ActiveTheme as _, TitleBar, v_flex};
use gpui_kit::*;

/// The main window's content: title bar above the library.
pub struct Workspace;

impl Workspace {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(TitleBar::new().child("Backdrop"))
            .child(
                v_flex().flex_1().min_h_0().p_4().child(
                    Empty::new().header(
                        EmptyHeader::new()
                            .title(EmptyTitle::new().child("No library folder"))
                            .description(
                                EmptyDescription::new()
                                    .child("Choose a folder of wallpapers to start browsing."),
                            ),
                    ),
                ),
            )
    }
}
