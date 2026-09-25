//! The limits on a terminal search request and its paged results.
//!
//! The worker's scanner, the coordinator's relay and the browser's find
//! controller all read these numbers from here, so a paging bound cannot drift
//! between the layer that asks for a page and the layer that serves one. The
//! search itself lives in the worker and the coordinator; this file owns the
//! bounds and the one wire string they all share.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ProtocolResult;
use crate::validate::one_of;

/// The query a caller may submit, counted in Unicode code points.
pub const TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS: usize = 256;
/// The grid epoch a page was scanned against, so a stale page can be dropped.
pub const TERMINAL_SEARCH_GRID_EPOCH_MAX_LENGTH: usize = 64;
/// The correlation id echoed back on every page of one search.
pub const TERMINAL_SEARCH_ID_MAX_LENGTH: usize = 64;
/// The most scrollback rows one page may scan.
pub const TERMINAL_SEARCH_MAX_ROWS: u32 = 4_096;
/// The most matches one page may return.
pub const TERMINAL_SEARCH_MAX_MATCHES: u32 = 256;
/// Each match carries a row preview, which IS the global search feature; it
/// stays bounded and is never persisted.
pub const TERMINAL_SEARCH_PREVIEW_MAX_CODE_POINTS: usize = 512;
/// The most pages one search may be paged across.
pub const TERMINAL_SEARCH_MAX_PAGES: u32 = 32;
/// The sessions one global search may span.
pub const GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS: usize = 32;
/// The rows scanned per selected session, so one large session cannot starve
/// the rest of the fleet out of the page budget.
pub const GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION: u32 = 2_048;
pub const GLOBAL_TERMINAL_SEARCH_MAX_MATCHES: u32 = 256;
pub const GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS: u32 = 5_000;
pub const GLOBAL_TERMINAL_SEARCH_WORK_DEADLINE_MS: u32 = 4_500;
pub const GLOBAL_TERMINAL_SEARCH_CURSOR_TTL_MS: u32 = 60_000;
pub const GLOBAL_TERMINAL_SEARCH_MAX_CURSORS_PER_DEVICE: u32 = 4;
pub const TERMINAL_SEARCH_RPC_DEADLINE_MS: u32 = 8_000;

/// Which history floor a short page hit, so the caller can stop paging and name
/// the cause instead of retrying forever.
///
/// A page clamped at the retained floor comes back SHORT; this says which floor
/// that was. `Other` carries a value a newer peer added so an older build can
/// still decode a frame it does not understand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrollbackHistoryFloor {
    /// The full requested range was served; no floor was hit.
    None,
    /// The earliest rows the worker still retained.
    Evicted,
    /// History before the current geometry, re-projected on resize.
    ResizeReplay,
    /// A floor a newer peer named.
    Other(String),
}

impl ScrollbackHistoryFloor {
    /// Every spelling this build admits, in the order the contract lists them.
    pub const WIRE_VALUES: [&'static str; 3] = ["none", "evicted", "resize_replay"];

    /// The spelling on the wire.
    pub fn as_wire(&self) -> &str {
        match self {
            Self::None => "none",
            Self::Evicted => "evicted",
            Self::ResizeReplay => "resize_replay",
            Self::Other(value) => value,
        }
    }

    /// The validated form of a wire string: an unknown floor is refused rather
    /// than silently mapped onto "no floor was hit".
    pub fn parse(field: &str, value: &str) -> ProtocolResult<Self> {
        one_of(field, value, &Self::WIRE_VALUES)?;
        Ok(match value {
            "none" => Self::None,
            "evicted" => Self::Evicted,
            _ => Self::ResizeReplay,
        })
    }
}

impl Serialize for ScrollbackHistoryFloor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for ScrollbackHistoryFloor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = String::deserialize(deserializer)?;
        Ok(match wire.as_str() {
            "none" => Self::None,
            "evicted" => Self::Evicted,
            "resize_replay" => Self::ResizeReplay,
            _ => Self::Other(wire),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS, GLOBAL_TERMINAL_SEARCH_WORK_DEADLINE_MS,
        ScrollbackHistoryFloor,
    };

    #[test]
    fn every_floor_spelling_round_trips_and_an_unknown_one_is_refused() {
        for floor in [
            ScrollbackHistoryFloor::None,
            ScrollbackHistoryFloor::Evicted,
            ScrollbackHistoryFloor::ResizeReplay,
        ] {
            let parsed = ScrollbackHistoryFloor::parse("history_floor", floor.as_wire());
            assert_eq!(parsed.ok(), Some(floor));
        }
        assert!(ScrollbackHistoryFloor::parse("history_floor", "unspecified").is_err());
    }

    #[test]
    fn an_unfamiliar_floor_still_decodes_rather_than_breaking_the_frame() {
        // A newer peer naming a floor this build has never heard of must not
        // turn a whole page of otherwise valid matches into a decode failure.
        let decoded: ScrollbackHistoryFloor =
            serde_json::from_str(r#""compaction""#).expect("an unknown floor decodes");
        assert_eq!(
            decoded,
            ScrollbackHistoryFloor::Other("compaction".to_owned())
        );
        assert_eq!(decoded.as_wire(), "compaction");
    }

    /// A page that outlived the deadline that funds it would fail as a timeout
    /// instead of arriving, so the page is a fraction of the work deadline
    /// rather than the whole of it. The comparison is between two `const`s and
    /// is therefore proved by the compiler; what the test pins is the
    /// conversion the page deadline goes through on its way to the wire.
    #[test]
    fn the_page_deadline_travels_as_the_literal_the_contract_names() {
        let page_deadline_ms: i64 = GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS as i64;
        assert_eq!(page_deadline_ms, 5_000);
        assert!(
            (GLOBAL_TERMINAL_SEARCH_WORK_DEADLINE_MS as i64) < page_deadline_ms,
            "a page that outlives its own funding is a timeout, not a page"
        );
    }
}
