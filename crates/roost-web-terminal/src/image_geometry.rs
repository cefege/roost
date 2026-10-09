//! Cell-space geometry for an inline terminal image.
//!
//! `CellGridRenderer` uses this value to paint images in the viewport's `ch`/`lh`
//! coordinate system. It depends only on the placement carried by the protocol.

use roost_protocol::cell::ImagePlacement;

/// Inline styles for one placed image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBoxStyle {
    /// Vertical position relative to the top of the viewport.
    pub top: String,
    /// Horizontal position relative to the left of the viewport.
    pub left: String,
    /// Width in terminal columns.
    pub width: String,
    /// Height in terminal rows.
    pub height: String,
    /// Background scaling for the selected source rectangle.
    pub background_size: String,
    /// Background offset for the selected source rectangle.
    pub background_position: String,
    /// Stacking order from the terminal image protocol.
    pub z_index: i32,
}

/// Convert a protocol placement to CSS in the terminal's cell coordinate space.
pub fn image_style(placement: &ImagePlacement, scrollback_total: u64) -> ImageBoxStyle {
    let viewport_row = i128::from(placement.row) - i128::from(scrollback_total);
    let offset_x = compact(f64::from(placement.offset_x_px) / 8.0);
    let offset_y = compact(f64::from(placement.offset_y_px) / 16.0);
    let size_x = percent(placement.image_width, placement.source_width);
    let size_y = percent(placement.image_height, placement.source_height);
    let position_x = percent(
        placement.source_x,
        placement.image_width.saturating_sub(placement.source_width),
    );
    let position_y = percent(
        placement.source_y,
        placement
            .image_height
            .saturating_sub(placement.source_height),
    );

    ImageBoxStyle {
        top: format!("calc({viewport_row} * 1lh + {offset_y} * 1lh)"),
        left: format!("calc({} * 1ch + {offset_x} * 1ch)", placement.col),
        width: format!("calc({} * 1ch)", placement.columns),
        height: format!("calc({} * 1lh)", placement.rows),
        background_size: format!("{size_x}% {size_y}%"),
        background_position: format!("{position_x}% {position_y}%"),
        z_index: placement.z_index,
    }
}

fn percent(numerator: u32, denominator: u32) -> String {
    if denominator == 0 {
        return "0".to_string();
    }
    compact(100.0 * f64::from(numerator) / f64::from(denominator))
}

fn compact(value: f64) -> String {
    let mut formatted = format!("{value:.4}");
    while formatted.ends_with('0') {
        formatted.pop();
    }
    if formatted.ends_with('.') {
        formatted.pop();
    }
    if formatted == "-0" {
        return "0".to_string();
    }
    formatted
}

#[cfg(test)]
mod tests {
    use super::image_style;
    use roost_protocol::cell::ImagePlacement;

    fn placement() -> ImagePlacement {
        ImagePlacement {
            image_key: 1,
            row: 2,
            col: 5,
            columns: 4,
            rows: 2,
            source_x: 150,
            source_y: 30,
            source_width: 400,
            source_height: 150,
            image_width: 1_000,
            image_height: 300,
            offset_x_px: 1,
            offset_y_px: 2,
            z_index: 0,
        }
    }

    #[test]
    fn source_crop_scales_and_positions_background() {
        let style = image_style(&placement(), 5);

        assert_eq!(style.background_size, "250% 200%");
        assert_eq!(style.background_position, "25% 20%");
    }

    #[test]
    fn history_rows_use_signed_offsets_and_compact_cell_fractions() {
        let style = image_style(&placement(), 5);

        assert_eq!(style.top, "calc(-3 * 1lh + 0.125 * 1lh)");
        assert_eq!(style.left, "calc(5 * 1ch + 0.125 * 1ch)");
    }

    #[test]
    fn zero_sized_source_dimensions_do_not_divide_by_zero() {
        let mut placement = placement();
        placement.source_width = 0;
        placement.source_height = 0;
        placement.image_width = 0;
        placement.image_height = 0;

        let style = image_style(&placement, 2);

        assert_eq!(style.background_size, "0% 0%");
        assert_eq!(style.background_position, "0% 0%");
    }
}
