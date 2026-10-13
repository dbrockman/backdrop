//! Graphical interface. Presentation only; behavior comes from `backdrop_core`.

mod api_key_form;
mod image_cache;
mod image_detail;
mod library_grid;
mod tiles;
mod wallhaven_browser;
mod wallhaven_detail;
mod workspace;

use std::sync::Arc;

use backdrop_core::paths::AppPaths;
use backdrop_core::providers::Wallhaven;
use gpui_kit::component::TitleBar;
use gpui_kit::*;

use image_detail::{Back, NextImage, PreviousImage};
use library_grid::{
    DeleteSelected, OpenSelected, SelectDown, SelectFirst, SelectLast, SelectLeft, SelectRight,
    SelectUp, SetWallpaper, ToggleFavorite,
};
use wallhaven_browser::{DownloadSelected, FocusSearch};
use workspace::{ChooseLibraryFolder, ShowLibrary, ShowWallhaven, Workspace};

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
                // Browsing shows safe-for-work wallpapers only, which need no API key.
                let wallhaven = Arc::new(Wallhaven::new(None));
                cx.new(|cx| {
                    Workspace::new(paths, backdrop_core::desktop::detect, wallhaven, window, cx)
                })
            })
            .expect("failed to open the main window");
            cx.activate(true);
        });
}

fn bind_keys(cx: &mut App) {
    let grid = library_grid::KEY_CONTEXT;
    let detail = image_detail::KEY_CONTEXT;
    let wallhaven = wallhaven_browser::KEY_CONTEXT;
    let wallhaven_detail = wallhaven_detail::KEY_CONTEXT;
    cx.bind_keys([
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("ctrl-o", ChooseLibraryFolder, Some(workspace::KEY_CONTEXT)),
        KeyBinding::new("ctrl-1", ShowLibrary, Some(workspace::KEY_CONTEXT)),
        KeyBinding::new("ctrl-2", ShowWallhaven, Some(workspace::KEY_CONTEXT)),
        KeyBinding::new("left", SelectLeft, Some(grid)),
        KeyBinding::new("right", SelectRight, Some(grid)),
        KeyBinding::new("up", SelectUp, Some(grid)),
        KeyBinding::new("down", SelectDown, Some(grid)),
        KeyBinding::new("home", SelectFirst, Some(grid)),
        KeyBinding::new("end", SelectLast, Some(grid)),
        KeyBinding::new("f", ToggleFavorite, Some(grid)),
        KeyBinding::new("enter", OpenSelected, Some(grid)),
        KeyBinding::new("w", SetWallpaper, Some(grid)),
        KeyBinding::new("w", SetWallpaper, Some(detail)),
        KeyBinding::new("delete", DeleteSelected, Some(grid)),
        KeyBinding::new("delete", DeleteSelected, Some(detail)),
        KeyBinding::new("escape", Back, Some(detail)),
        KeyBinding::new("left", PreviousImage, Some(detail)),
        KeyBinding::new("right", NextImage, Some(detail)),
        KeyBinding::new("f", ToggleFavorite, Some(detail)),
        KeyBinding::new("left", SelectLeft, Some(wallhaven)),
        KeyBinding::new("right", SelectRight, Some(wallhaven)),
        KeyBinding::new("up", SelectUp, Some(wallhaven)),
        KeyBinding::new("down", SelectDown, Some(wallhaven)),
        KeyBinding::new("home", SelectFirst, Some(wallhaven)),
        KeyBinding::new("end", SelectLast, Some(wallhaven)),
        KeyBinding::new("d", DownloadSelected, Some(wallhaven)),
        KeyBinding::new("ctrl-f", FocusSearch, Some(wallhaven)),
        KeyBinding::new("enter", OpenSelected, Some(wallhaven)),
        KeyBinding::new("escape", Back, Some(wallhaven_detail)),
        KeyBinding::new("left", PreviousImage, Some(wallhaven_detail)),
        KeyBinding::new("right", NextImage, Some(wallhaven_detail)),
        KeyBinding::new("d", DownloadSelected, Some(wallhaven_detail)),
    ]);
}
