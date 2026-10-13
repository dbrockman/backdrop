//! Graphical interface. Presentation only; behavior comes from `backdrop_core`.

mod format;
mod image_cache;
mod image_detail;
mod library_grid;
mod workspace;

use backdrop_core::paths::AppPaths;
use gpui_kit::component::TitleBar;
use gpui_kit::*;

use image_detail::{Back, NextImage, PreviousImage};
use library_grid::{
    OpenSelected, SelectDown, SelectFirst, SelectLast, SelectLeft, SelectRight, SelectUp,
    ToggleFavorite,
};
use workspace::{ChooseLibraryFolder, Workspace};

const APP_ID: &str = "backdrop";
const APP_NAME: &str = "Backdrop";

actions!(backdrop, [Quit]);

pub fn run(paths: AppPaths) {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            bind_keys(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());
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
                titlebar: Some(TitlebarOptions {
                    title: Some(APP_NAME.into()),
                    ..TitleBar::title_bar_options()
                }),
                ..TitleBar::window_options()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Workspace::new(paths, window, cx))
            })
            .expect("failed to open the main window");
            cx.activate(true);
        });
}

fn bind_keys(cx: &mut App) {
    let grid = library_grid::KEY_CONTEXT;
    let detail = image_detail::KEY_CONTEXT;
    cx.bind_keys([
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("ctrl-o", ChooseLibraryFolder, Some(workspace::KEY_CONTEXT)),
        KeyBinding::new("left", SelectLeft, Some(grid)),
        KeyBinding::new("right", SelectRight, Some(grid)),
        KeyBinding::new("up", SelectUp, Some(grid)),
        KeyBinding::new("down", SelectDown, Some(grid)),
        KeyBinding::new("home", SelectFirst, Some(grid)),
        KeyBinding::new("end", SelectLast, Some(grid)),
        KeyBinding::new("f", ToggleFavorite, Some(grid)),
        KeyBinding::new("enter", OpenSelected, Some(grid)),
        KeyBinding::new("escape", Back, Some(detail)),
        KeyBinding::new("left", PreviousImage, Some(detail)),
        KeyBinding::new("right", NextImage, Some(detail)),
        KeyBinding::new("f", ToggleFavorite, Some(detail)),
    ]);
}
