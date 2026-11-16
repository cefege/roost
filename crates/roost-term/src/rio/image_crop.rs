//! Atlas crops use display pixels; frame placements use original image pixels.
//! The rio image adapter converts each axis before publishing a source rectangle.
//! Integer bounds retain every source pixel touched by a surviving cell fragment.

use rio_vt::ansi::graphics::AtlasPlacement;

pub(super) fn atlas_source_crop(
    placement: &AtlasPlacement,
    image_width: u32,
    image_height: u32,
) -> (u32, u32, u32, u32) {
    let (source_x, source_width) = source_axis(
        placement.src_x,
        placement.src_width,
        placement.total_width,
        image_width,
    );
    let (source_y, source_height) = source_axis(
        placement.src_y,
        placement.src_height,
        placement.total_height,
        image_height,
    );
    (source_x, source_y, source_width, source_height)
}

fn source_axis(start: u32, length: u32, display: u32, pixels: u32) -> (u32, u32) {
    if display == 0 || length == 0 {
        return (0, 0);
    }
    let scale = u64::from(pixels);
    let denominator = u64::from(display);
    let first = (u64::from(start) * scale / denominator).min(scale);
    let last = (u64::from(start.saturating_add(length)) * scale)
        .div_ceil(denominator)
        .min(scale);
    (first as u32, last.saturating_sub(first) as u32)
}
