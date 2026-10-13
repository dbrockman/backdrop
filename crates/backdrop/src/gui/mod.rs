//! Graphical interface. Presentation only; behavior comes from `backdrop_core`.

mod workspace;

use backdrop_core::paths::AppPaths;
use gpui_kit::component::TitleBar;
use gpui_kit::*;

use workspace::{ChooseLibraryFolder, Workspace};

const APP_ID: &str = "backdrop";

actions!(backdrop, [Quit]);

pub fn run(paths: AppPaths) {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);

            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.bind_keys([
                KeyBinding::new("ctrl-q", Quit, None),
                KeyBinding::new("ctrl-o", ChooseLibraryFolder, Some(workspace::KEY_CONTEXT)),
            ]);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1200.), px(800.)), cx)),
                window_min_size: Some(size(px(640.), px(400.))),
                app_id: Some(APP_ID.into()),
                ..TitleBar::window_options()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Workspace::new(paths, window, cx))
            })
            .expect("failed to open the main window");
            cx.activate(true);
        });
}
