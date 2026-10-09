//! An inline image's position on the cell grid, as a frame carries it.
//!
//! The worker's terminal core decodes kitty graphics, sixel and iTerm2 images;
//! a frame carries only WHERE each image sits and which part of it shows. The
//! pixels are fetched once per `image_key` (`SessionsGetTerminalImage`, or the
//! direct carriers' image request) and cached by the browser.

use std::sync::Arc;

/// One image placed on the grid.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImagePlacement {
    /// Content identity of the image's pixels: equal pixels, equal key, so a
    /// browser fetches each image once however often it is placed.
    pub image_key: u64,
    /// Absolute row of the placement's top edge, in the frame's row space:
    /// viewport row `v` is `scrollback_total + v`, history rows are below it.
    pub row: u64,
    /// Column of the placement's left edge.
    pub col: u16,
    /// Cells the placement spans; never 0.
    pub columns: u16,
    pub rows: u16,
    /// The source rectangle of the image shown, in image pixels.
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    /// The whole image's pixel size.
    pub image_width: u32,
    pub image_height: u32,
    /// Offset of the image inside its first cell, in the core's nominal cell
    /// pixels (8×16).
    pub offset_x_px: u16,
    pub offset_y_px: u16,
    /// Stacking: negative paints below the text, otherwise above it.
    pub z_index: i32,
}

/// The placements a frame carries, shared between the frames that hold them.
pub type ImagePlacements = Arc<[ImagePlacement]>;

/// The most placements one frame carries; a decoder refuses more.
pub const MAX_IMAGE_PLACEMENTS: usize = 256;

/// An empty placement set, what a full frame with no images carries.
pub fn no_image_placements() -> ImagePlacements {
    Arc::from([])
}
