//! The coordinator methods one terminal pane calls directly: a history page for
//! its scrollback pager, and the cursor position its peers' ghosts follow.
//!
//! Called by roost-web's `components::terminal` pane mount through
//! `CoordRpc::call`. v2 call sites: `apps/web/src/lib/scrollbackDirectHistory.ts:30`
//! (`sessionsGetScrollbackCells`) and
//! `apps/web/src/components/terminal/cell-terminal-renderer.ts:338` (`sessionsCursorPos`).

use roost_proto::{
    ScrollbackHistoryFloor as PbHistoryFloor, SessionsCursorPosRequest, SessionsCursorPosResponse,
    SessionsGetScrollbackCellsRequest, SessionsGetScrollbackCellsResponse,
};
use roost_protocol::cell::CellRow;
use roost_protocol::cell::proto::cell_row_from_proto;
use roost_protocol::terminal_search::ScrollbackHistoryFloor;

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// `SessionsGetScrollbackCells`: one page of immutable history, named by its
/// NEWEST row, fenced to the grid epoch the pane painted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollbackCells {
    /// The session whose history is read.
    pub session_id: String,
    /// The row after the newest one wanted (exclusive).
    pub end_row: u64,
    /// The most rows the page may carry.
    pub max_rows: u32,
    /// The grid numbering the rows must belong to.
    pub grid_epoch: String,
}

/// What `SessionsGetScrollbackCells` answered, rows decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollbackCellsPage {
    /// The rows, oldest first, each carrying its absolute index.
    pub rows: Vec<CellRow>,
    /// The columns the rows were laid out in.
    pub cols: u32,
    /// The retained scrollback lines the worker holds now.
    pub scrollback_total: u64,
    /// The first row served, inclusive.
    pub start_row: u64,
    /// The row after the last one served.
    pub end_row: u64,
    /// The grid numbering the rows belong to.
    pub grid_epoch: String,
    /// Which retention floor a short page hit.
    pub history_floor: ScrollbackHistoryFloor,
}

impl UnaryMethod for ScrollbackCells {
    const METHOD: &'static str = "SessionsGetScrollbackCells";
    type Response = ScrollbackCellsPage;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &SessionsGetScrollbackCellsRequest {
                session_id: self.session_id.clone(),
                end_row: self.end_row,
                max_rows: self.max_rows,
                grid_epoch: self.grid_epoch.clone(),
                ..Default::default()
            },
        )
    }

    /// A row the cell model refuses fails the whole page: the pager splices a
    /// page only when it names every row of its range, so a page with a hole
    /// is not a shorter page, it is a wrong one.
    fn decode_response(body: &[u8]) -> Result<ScrollbackCellsPage, RpcCodecError> {
        let response: SessionsGetScrollbackCellsResponse = decode_message(Self::METHOD, body)?;
        let rows = response
            .rows
            .iter()
            .map(cell_row_from_proto)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| RpcCodecError::MalformedResponse {
                method: Self::METHOD,
                detail: error.to_string(),
            })?;
        Ok(ScrollbackCellsPage {
            rows,
            cols: response.cols,
            scrollback_total: response.scrollback_total,
            start_row: response.start_row,
            end_row: response.end_row,
            grid_epoch: response.grid_epoch,
            history_floor: history_floor(response.history_floor.as_known()),
        })
    }
}

/// An unknown floor reads as "no floor claimed": the pager then keeps paging
/// instead of declaring history gone on a value it cannot interpret.
fn history_floor(floor: Option<PbHistoryFloor>) -> ScrollbackHistoryFloor {
    match floor {
        Some(PbHistoryFloor::SCROLLBACK_HISTORY_FLOOR_EVICTED) => ScrollbackHistoryFloor::Evicted,
        Some(PbHistoryFloor::SCROLLBACK_HISTORY_FLOOR_RESIZE_REPLAY) => {
            ScrollbackHistoryFloor::ResizeReplay
        }
        None | Some(PbHistoryFloor::SCROLLBACK_HISTORY_FLOOR_UNSPECIFIED) => {
            ScrollbackHistoryFloor::None
        }
    }
}

/// `SessionsCursorPos`: where this viewer's cursor is, for other viewers'
/// ghost cursors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorPos {
    /// The session.
    pub session_id: String,
    /// The cursor column.
    pub col: u32,
    /// The cursor row.
    pub row: u32,
}

impl UnaryMethod for CursorPos {
    const METHOD: &'static str = "SessionsCursorPos";
    /// Whether the coordinator accepted the position.
    type Response = bool;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &SessionsCursorPosRequest {
                session_id: self.session_id.clone(),
                col: self.col,
                row: self.row,
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<bool, RpcCodecError> {
        let response: SessionsCursorPosResponse = decode_message(Self::METHOD, body)?;
        Ok(response.accepted)
    }
}
