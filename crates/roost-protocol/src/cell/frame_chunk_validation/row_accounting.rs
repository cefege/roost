//! Per-row span and hyperlink accounting inside one snapshot part.
//!
//! The parent validates the part's scalars and the shape of the snapshot; this
//! module owns what happens once a row is in hand — its spans are charged
//! against the snapshot cap, and the hyperlink runs it introduces are interned.
//! A key that maps to two URIs is a conflict, because a renderer groups a
//! wrapped link by that key.

use std::collections::HashMap;

use roost_proto::PbCellRow;

use super::{CELL_GRID_SNAPSHOT_MAX_LINK_MAPPINGS, CELL_GRID_SNAPSHOT_MAX_SPANS};
use crate::cell::frame_chunk_validation::CellGridChunkErrorCode as Code;
use crate::cell::frame_chunk_validation::CellGridLinkMapping;
use crate::cell::frame_chunk_validation::reject_cell_grid_chunk as reject;
use crate::cell::frame_chunk_validation::{CellGridChunkError, quoted};
use crate::cell::proto::cell_row_from_proto_bounded;

/// One row's spans, checked by the single owner of the span contract. A
/// violation reports as a span fault: it is the only span-family code, and no
/// consumer branches on which code a refusal carries.
pub(super) fn assert_part_row_spans(
    row: &PbCellRow,
    max_columns: u32,
) -> Result<(), CellGridChunkError> {
    cell_row_from_proto_bounded(row, max_columns)
        .map(|_| ())
        .map_err(|error| reject(Code::SpanLimit, error.reason))
}

/// Charge one row's spans against the snapshot cap.
pub(super) fn account_spans(spans: u32, row: &PbCellRow) -> Result<u32, CellGridChunkError> {
    let spans = spans.saturating_add(u32::try_from(row.spans.len()).unwrap_or(u32::MAX));
    if spans > CELL_GRID_SNAPSHOT_MAX_SPANS {
        return Err(reject(
            Code::SpanLimit,
            format!("snapshot has more than {CELL_GRID_SNAPSHOT_MAX_SPANS} spans"),
        ));
    }
    Ok(spans)
}

/// Intern the hyperlink runs a row introduces. A key already seen must point at
/// the same URI, or the click it names resolves to two destinations.
pub(super) fn record_span_links(
    row: &PbCellRow,
    links: &mut HashMap<String, CellGridLinkMapping>,
) -> Result<(), CellGridChunkError> {
    for span in &row.spans {
        let (Some(key), Some(uri)) = (&span.link_key, &span.link_uri) else {
            continue;
        };
        if let Some(prior) = links.get(key) {
            if &prior.uri != uri {
                return Err(reject(
                    Code::LinkConflict,
                    format!("link_key {} maps to conflicting URIs", quoted(key)),
                ));
            }
            continue;
        }
        if links.len() >= CELL_GRID_SNAPSHOT_MAX_LINK_MAPPINGS {
            let ceiling = CELL_GRID_SNAPSHOT_MAX_LINK_MAPPINGS;
            let reason = format!("snapshot has more than {ceiling} link mappings");
            return Err(reject(Code::LinkLimit, reason));
        }
        let mapping = CellGridLinkMapping {
            key: key.clone(),
            uri: uri.clone(),
        };
        links.insert(key.clone(), mapping);
    }
    Ok(())
}
