//! Image placements across the wire boundary: `ImagePlacement` to
//! `PbImagePlacement` and back. `cell::proto` calls both for every frame. The
//! decode refuses what a renderer could not place — too many placements, an
//! empty span, or a wire value past its 16-bit field — rather than clamping.

use std::sync::Arc;

use roost_proto::{PbCellGridFrame, PbImagePlacement};

use crate::cell::types::{ImagePlacement, ImagePlacements, MAX_IMAGE_PLACEMENTS};
use crate::error::{ProtocolError, ProtocolResult};

/// The frame's placements as the wire spells them, plus the presence flag.
pub(crate) fn placements_to_proto(
    placements: Option<&ImagePlacements>,
) -> (Vec<PbImagePlacement>, bool) {
    let Some(placements) = placements else {
        return (Vec::new(), false);
    };
    let encoded = placements
        .iter()
        .map(|placement| PbImagePlacement {
            image_key: placement.image_key,
            row: placement.row,
            col: u32::from(placement.col),
            columns: u32::from(placement.columns),
            rows: u32::from(placement.rows),
            source_x: placement.source_x,
            source_y: placement.source_y,
            source_width: placement.source_width,
            source_height: placement.source_height,
            image_width: placement.image_width,
            image_height: placement.image_height,
            offset_x_px: u32::from(placement.offset_x_px),
            offset_y_px: u32::from(placement.offset_y_px),
            z_index: placement.z_index,
            ..Default::default()
        })
        .collect();
    (encoded, true)
}

/// A wire frame's placements, `None` when a delta did not carry a set. A full
/// frame always has one: a full without the flag states that it has no images.
pub(crate) fn placements_from_proto(
    frame: &PbCellGridFrame,
) -> ProtocolResult<Option<ImagePlacements>> {
    if !frame.image_placements_present {
        return Ok(frame.full.then(|| Arc::from([])));
    }
    if frame.image_placements.len() > MAX_IMAGE_PLACEMENTS {
        return Err(ProtocolError::new(
            "cell_frame.image_placements",
            format!(
                "{} placements exceed the {MAX_IMAGE_PLACEMENTS} a frame may carry",
                frame.image_placements.len()
            ),
        ));
    }
    let decoded = frame
        .image_placements
        .iter()
        .enumerate()
        .map(|(position, placement)| placement_from_proto(position, placement))
        .collect::<ProtocolResult<Vec<_>>>()?;
    Ok(Some(Arc::from(decoded)))
}

fn placement_from_proto(
    position: usize,
    placement: &PbImagePlacement,
) -> ProtocolResult<ImagePlacement> {
    let narrow = |name: &str, value: u32| {
        u16::try_from(value).map_err(|_| {
            ProtocolError::new(
                format!("cell_frame.image_placements[{position}].{name}"),
                format!("{value} is outside the 16-bit cell range"),
            )
        })
    };
    let columns = narrow("columns", placement.columns)?;
    let rows = narrow("rows", placement.rows)?;
    if columns == 0 || rows == 0 {
        return Err(ProtocolError::new(
            format!("cell_frame.image_placements[{position}]"),
            "a placement spans at least one cell in each direction",
        ));
    }
    Ok(ImagePlacement {
        image_key: placement.image_key,
        row: placement.row,
        col: narrow("col", placement.col)?,
        columns,
        rows,
        source_x: placement.source_x,
        source_y: placement.source_y,
        source_width: placement.source_width,
        source_height: placement.source_height,
        image_width: placement.image_width,
        image_height: placement.image_height,
        offset_x_px: narrow("offset_x_px", placement.offset_x_px)?,
        offset_y_px: narrow("offset_y_px", placement.offset_y_px)?,
        z_index: placement.z_index,
    })
}
