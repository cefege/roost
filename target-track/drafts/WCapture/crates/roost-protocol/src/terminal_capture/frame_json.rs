//! The JSON projection of a cell frame, row and span exactly as v2's
//! `JSON.stringify` spelled them inside an incident bundle: camelCase members,
//! `mouseTracking` as its wire integer, and the optional colour and link
//! members omitted when absent. Used by [`super::bundle`]'s serializers; the
//! bundle validator in [`super::validate`] reads back this same shape.

use std::sync::Arc;

use serde::ser::{SerializeSeq, SerializeStruct};
use serde::{Serialize, Serializer};

use crate::cell::{CellGridFrame, CellRow, CellSpan};

/// One frame, as a bundle carries it.
#[derive(Debug, Clone, Copy)]
pub struct FrameJson<'a>(pub &'a CellGridFrame);

/// One row, as a bundle carries it.
#[derive(Debug, Clone, Copy)]
pub struct RowJson<'a>(pub &'a CellRow);

#[derive(Debug, Clone, Copy)]
struct SpanJson<'a>(&'a CellSpan);

#[derive(Debug, Clone, Copy)]
struct RowsJson<'a>(&'a [CellRow]);

impl Serialize for FrameJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let frame = self.0;
        let mut out = serializer.serialize_struct("CellGridFrame", 22)?;
        out.serialize_field("streamId", &frame.stream_id)?;
        out.serialize_field("gridEpoch", &frame.grid_epoch)?;
        out.serialize_field("cols", &frame.cols)?;
        out.serialize_field("rows", &frame.rows)?;
        out.serialize_field("cursorRow", &frame.cursor_row)?;
        out.serialize_field("cursorCol", &frame.cursor_col)?;
        out.serialize_field("cursorVisible", &frame.cursor_visible)?;
        out.serialize_field("altScreen", &frame.alt_screen)?;
        out.serialize_field("cursorKeysApp", &frame.cursor_keys_app)?;
        out.serialize_field("bracketedPaste", &frame.bracketed_paste)?;
        out.serialize_field("mouseTracking", &u32::from(frame.mouse_tracking))?;
        out.serialize_field("mouseSgr", &frame.mouse_sgr)?;
        out.serialize_field("focusEvents", &frame.focus_events)?;
        out.serialize_field("full", &frame.full)?;
        out.serialize_field("viewportRows", &RowsJson(&frame.viewport_rows))?;
        out.serialize_field("scrollbackRows", &RowsJson(&frame.scrollback_rows))?;
        out.serialize_field("scrollbackAppend", &RowsJson(&frame.scrollback_append))?;
        out.serialize_field("scrollbackTotal", &frame.scrollback_total)?;
        out.serialize_field("sbBase", &frame.sb_base)?;
        out.serialize_field("baseSeq", &frame.base_seq)?;
        out.serialize_field("seq", &frame.seq)?;
        out.end()
    }
}

impl Serialize for RowJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let row = self.0;
        let mut out = serializer.serialize_struct("CellRow", 2)?;
        out.serialize_field("index", &row.index)?;
        out.serialize_field("spans", &SpansJson(&row.spans))?;
        out.end()
    }
}

impl Serialize for RowsJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_seq(Some(self.0.len()))?;
        for row in self.0 {
            out.serialize_element(&RowJson(row))?;
        }
        out.end()
    }
}

#[derive(Debug, Clone, Copy)]
struct SpansJson<'a>(&'a [CellSpan]);

impl Serialize for SpansJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_seq(Some(self.0.len()))?;
        for span in self.0 {
            out.serialize_element(&SpanJson(span))?;
        }
        out.end()
    }
}

impl Serialize for SpanJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let span = self.0;
        let mut out = serializer.serialize_struct("CellSpan", 9)?;
        out.serialize_field("text", &span.text)?;
        out.serialize_field("fg", &span.fg)?;
        out.serialize_field("bg", &span.bg)?;
        out.serialize_field("flags", &span.flags)?;
        match span.fg_rgb {
            Some(rgb) => out.serialize_field("fgRgb", &rgb)?,
            None => out.skip_field("fgRgb")?,
        }
        match span.bg_rgb {
            Some(rgb) => out.serialize_field("bgRgb", &rgb)?,
            None => out.skip_field("bgRgb")?,
        }
        out.serialize_field("columns", &span.columns)?;
        match &span.link_uri {
            Some(uri) => out.serialize_field("linkUri", uri)?,
            None => out.skip_field("linkUri")?,
        }
        match &span.link_key {
            Some(key) => out.serialize_field("linkKey", key)?,
            None => out.skip_field("linkKey")?,
        }
        out.end()
    }
}

/// `serialize_with` adaptor for a retained frame.
pub fn serialize_shared_frame<S: Serializer>(
    frame: &Arc<CellGridFrame>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    FrameJson(frame).serialize(serializer)
}

/// `serialize_with` adaptor for a row list.
pub fn serialize_rows<S: Serializer>(rows: &[CellRow], serializer: S) -> Result<S::Ok, S::Error> {
    RowsJson(rows).serialize(serializer)
}
