//! The window arithmetic behind `SessionsGetScrollbackCells`.
//!
//! The coordinator does not hold a session's scrollback: the SPA holds only a
//! bounded window of the authoritative grid, so a history page is asked of the
//! worker and relayed here. What the coordinator owns is the RANGE -- which
//! absolute rows a request names, and what a short page means -- because a
//! caller that pages wrong walks off the end of a grid it cannot see.
//!
//! `roost-worker`'s `scrollback_read` owns the worker's own read of the same
//! range. These are two implementations of one arithmetic on either side of a
//! process boundary, and neither crate imports the other.

use connectrpc::{ConnectError, ErrorCode};

/// The largest absolute row index JSON can carry without losing precision.
pub const MAX_SAFE_ROW: u64 = 9_007_199_254_740_991;

/// The half-open absolute range a page asks the worker for.
///
/// `end_row` is exclusive and `max_rows` bounds how far back it reaches, so a
/// caller paging backwards from `end_row` gets the `max_rows` rows immediately
/// before it. The floor is 0 because a session that has never scrolled has no
/// row below zero, and a negative start would be a row no core can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbackWindow {
    /// Inclusive first absolute row of the request.
    pub start_row: u64,
    /// Exclusive last absolute row of the request.
    pub end_row: u64,
}

impl ScrollbackWindow {
    /// The window a request names, or the refusal a malformed row index earns.
    pub fn for_request(end_row: u64, max_rows: u32, field: &str) -> Result<Self, ConnectError> {
        require_json_safe_row(end_row, field)?;
        Ok(Self {
            start_row: end_row.saturating_sub(u64::from(max_rows)),
            end_row,
        })
    }

    /// How many rows this window names, after the floor clamped it.
    #[must_use]
    pub fn row_count(self) -> u64 {
        self.end_row.saturating_sub(self.start_row)
    }

    /// Whether a worker answer stayed INSIDE the requested range.
    ///
    /// A page that comes back SHORT is legal, and is not this method's
    /// business: the worker clamps at whatever history it still retains, and
    /// names that clamp in `history_floor` precisely so a caller can stop
    /// paging and say which floor it hit. Refusing a short page would make the
    /// floor undecodable -- the one case it exists to describe.
    ///
    /// A page that comes back LONGER is the defect: rows past the exclusive
    /// end the caller named are rows this request did not ask about, and a
    /// browser numbering them against its own cursor would skip history.
    #[must_use]
    pub fn is_served_by(self, start_row: u64, end_row: u64) -> bool {
        end_row <= self.end_row && start_row <= end_row
    }
}

/// A row index that survives a JSON round trip.
///
/// The worker answer and the response both carry these as JSON numbers, and a
/// value past 2^53-1 is a row no browser can address, so it is refused at the
/// boundary rather than silently rounded into one it can.
pub fn require_json_safe_row(value: u64, field: &str) -> Result<(), ConnectError> {
    if value > MAX_SAFE_ROW {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            format!("{field} must be a nonnegative JSON-safe integer"),
        ));
    }
    Ok(())
}

/// The order the served rows must arrive in.
///
/// The proto says "oldest to newest, absolute index set", and a page that
/// arrives newest-first would make the browser scroll backwards through a
/// terminal. Both the order and the absolute numbering are checked together: a
/// contiguous run that starts at the wrong row is the same defect as one that
/// runs backwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrollbackRowOrder {
    /// Every row arrived exactly once, ascending, starting at the served row.
    Ordered,
    /// The answer is not a page this RPC will serve.
    Refused(&'static str),
}

/// Check one served window's rows for order, contiguity and absolute numbering.
#[must_use]
pub fn check_row_order(served_start_row: u64, row_indices: &[u32]) -> ScrollbackRowOrder {
    if row_indices.is_empty() {
        return ScrollbackRowOrder::Ordered;
    }
    if row_indices.len() > u64::BITS as usize {
        return ScrollbackRowOrder::Refused(
            "scrollback page carries more rows than a row index can name",
        );
    }
    for (offset, index) in row_indices.iter().enumerate() {
        let expected = served_start_row.saturating_add(offset as u64);
        if u64::from(*index) != expected {
            return ScrollbackRowOrder::Refused(
                "scrollback page rows are not an ascending contiguous run from start_row",
            );
        }
    }
    ScrollbackRowOrder::Ordered
}
