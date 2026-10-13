//! Geometry shared by the thumbnail grids, so library and provider tiles have
//! the same size, spacing, and selection ring.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

/// Tile geometry, in rems so it follows the UI scale.
const MIN_TILE_WIDTH: f32 = 13.;
pub const GAP: f32 = 0.75;
pub const PADDING: f32 = 1.;
/// Tiles are 16:10, the most common wallpaper shape; others are cropped.
const TILE_ASPECT: f32 = 10. / 16.;
/// How far the selection ring sits outside the tile. Must stay below the gap
/// and the padding so the ring is never clipped.
const SELECTION_OFFSET: f32 = 0.25;

/// How many tiles fit side by side in a grid `width` wide.
pub fn columns(width: Pixels, rem: Pixels) -> usize {
    let available = width - rem * (2. * PADDING) + rem * GAP;
    ((available / (rem * (MIN_TILE_WIDTH + GAP))).floor() as usize).max(1)
}

/// The size of each tile when `columns` tiles fill a grid `width` wide.
pub fn tile_size(width: Pixels, columns: usize, rem: Pixels) -> Size<Pixels> {
    let gap = rem * GAP;
    let tile_width =
        ((width - rem * (2. * PADDING) - gap * (columns as f32 - 1.)) / columns as f32).max(px(1.));
    size(tile_width, tile_width * TILE_ASPECT)
}

/// The ring around the selected tile. It sits outside the picture, in the gap
/// between tiles, so it never covers it; its radius grows by the same offset
/// so both corners stay concentric.
pub fn selection_ring(rem: Pixels, cx: &App) -> Div {
    let theme = cx.theme();
    let offset = rem * SELECTION_OFFSET;
    div()
        .absolute()
        .top(-offset)
        .left(-offset)
        .right(-offset)
        .bottom(-offset)
        .rounded(theme.radius + offset)
        .border_2()
        .border_color(theme.primary)
}

/// A small label over a corner of a tile's picture, readable on any image.
/// Labelled in words, so it never relies on color alone.
pub fn picture_badge(label: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .absolute()
        .top_1p5()
        .left_1p5()
        .px_2()
        .py_0p5()
        .rounded_full()
        .bg(theme.background.opacity(0.85))
        .text_xs()
        .child(label.into())
}
