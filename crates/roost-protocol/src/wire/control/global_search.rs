//! The bounded fields a terminal-search control frame names: the query, the
//! grid epoch it is scanned against, the search id it is correlated by, and the
//! exclusive row cursor it pages from.
//!
//! `control` owns the frames; this file owns what may go in them, so a bound is
//! stated once. Every limit is read from `terminal_search`, which is the table
//! the worker scanner, the coordinator relay and the browser's find controller
//! already share — restating a number here is how a page size ends up different
//! on the machine that serves the page from the machine that asks for it.
//!
//! Every type checks on decode, not on use: a bounded string that only fails
//! when a scanner reads it is a frame that was already accepted.

pub mod batch;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::terminal_search::{
    TERMINAL_SEARCH_GRID_EPOCH_MAX_LENGTH, TERMINAL_SEARCH_ID_MAX_LENGTH,
    TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS,
};
use crate::validate::{integer_in_range, max_utf8_bytes, non_empty};
use crate::{ProtocolError, ProtocolResult};

pub use batch::{GlobalSearchSession, GlobalSearchSessionIds, GlobalSearchSessions};

/// The largest integer a JavaScript peer can have sent without losing
/// precision, and the cap on an absolute row index. A cursor past it is a
/// producer that is not counting rows at all, and a value that could not
/// round-trip through a browser is worse than a rejected one.
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// The text a search looks for, counted in Unicode code points.
///
/// Code points, not UTF-16 code units and not bytes: a query of astral
/// characters is a legitimate needle, and a cap counted the way a browser
/// stores its strings would admit half as many of them as the contract says.
/// An empty query is legal — a caller pages to the newest boundary without
/// filtering, and the frame is how it says so.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TerminalSearchQuery(String);

impl TerminalSearchQuery {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for TerminalSearchQuery {
    type Error = ProtocolError;

    fn try_from(value: &str) -> ProtocolResult<Self> {
        let over_limit = value
            .chars()
            .nth(TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS)
            .is_some();
        if over_limit {
            return Err(ProtocolError::new(
                "query",
                format!(
                    "must not exceed {TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS} Unicode code points"
                ),
            ));
        }
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for TerminalSearchQuery {
    type Error = ProtocolError;

    fn try_from(value: String) -> ProtocolResult<Self> {
        TerminalSearchQuery::try_from(value.as_str())?;
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for TerminalSearchQuery {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        TerminalSearchQuery::try_from(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// The grid epoch a page was scanned against, so a page from a core that has
/// since been rebuilt is dropped instead of painted onto the wrong rows.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TerminalSearchGridEpoch(String);

impl TerminalSearchGridEpoch {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for TerminalSearchGridEpoch {
    type Error = ProtocolError;

    fn try_from(value: &str) -> ProtocolResult<Self> {
        max_utf8_bytes("grid_epoch", value, TERMINAL_SEARCH_GRID_EPOCH_MAX_LENGTH)?;
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for TerminalSearchGridEpoch {
    type Error = ProtocolError;

    fn try_from(value: String) -> ProtocolResult<Self> {
        TerminalSearchGridEpoch::try_from(value.as_str())?;
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for TerminalSearchGridEpoch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        TerminalSearchGridEpoch::try_from(String::deserialize(deserializer)?)
            .map_err(D::Error::custom)
    }
}

/// The correlation id echoed back on every page of one search.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TerminalSearchId(String);

impl TerminalSearchId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for TerminalSearchId {
    type Error = ProtocolError;

    fn try_from(value: &str) -> ProtocolResult<Self> {
        non_empty("search_id", value)?;
        max_utf8_bytes("search_id", value, TERMINAL_SEARCH_ID_MAX_LENGTH)?;
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for TerminalSearchId {
    type Error = ProtocolError;

    fn try_from(value: String) -> ProtocolResult<Self> {
        TerminalSearchId::try_from(value.as_str())?;
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for TerminalSearchId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        TerminalSearchId::try_from(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// An absolute row index inside one grid epoch. A page names its start with
/// this as an exclusive cursor, so a paged search never re-reads the row the
/// last page ended on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TerminalSearchRow(i64);

impl TerminalSearchRow {
    pub fn as_i64(self) -> i64 {
        self.0
    }
}

impl TryFrom<i64> for TerminalSearchRow {
    type Error = ProtocolError;

    fn try_from(value: i64) -> ProtocolResult<Self> {
        integer_in_range("before_row", value, 0, MAX_SAFE_INTEGER)?;
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for TerminalSearchRow {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        TerminalSearchRow::try_from(i64::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS, TerminalSearchGridEpoch, TerminalSearchId,
        TerminalSearchQuery, TerminalSearchRow,
    };

    const ASTRAL: &str = "\u{1f642}";

    #[test]
    fn a_query_is_capped_in_code_points_not_in_the_units_a_browser_stores() {
        let at_limit: String = ASTRAL.repeat(TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS);
        assert_eq!(at_limit.encode_utf16().count(), 512);
        assert!(TerminalSearchQuery::try_from(at_limit.as_str()).is_ok());
        assert!(TerminalSearchQuery::try_from(format!("{at_limit}{ASTRAL}").as_str()).is_err());
        assert!(TerminalSearchQuery::try_from("").is_ok());
    }

    #[test]
    fn an_epoch_and_an_id_admit_their_last_character_and_refuse_the_next() {
        let epoch = "e".repeat(64);
        assert!(TerminalSearchGridEpoch::try_from(epoch.as_str()).is_ok());
        assert!(TerminalSearchGridEpoch::try_from(format!("{epoch}e").as_str()).is_err());
        let id = "i".repeat(64);
        assert!(TerminalSearchId::try_from(id.as_str()).is_ok());
        assert!(TerminalSearchId::try_from(format!("{id}i").as_str()).is_err());
        assert!(TerminalSearchId::try_from("").is_err());
    }

    #[test]
    fn a_row_cursor_is_a_nonnegative_safe_integer() {
        assert_eq!(
            TerminalSearchRow::try_from(0)
                .ok()
                .map(TerminalSearchRow::as_i64),
            Some(0)
        );
        assert!(TerminalSearchRow::try_from(-1).is_err());
        assert!(TerminalSearchRow::try_from(9_007_199_254_740_991).is_ok());
        assert!(TerminalSearchRow::try_from(9_007_199_254_740_992).is_err());
    }
}
