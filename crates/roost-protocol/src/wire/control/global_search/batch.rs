//! The two session lists a fleet-wide search carries: the sessions to scan and
//! the sessions a cancel stops.
//!
//! Both are bounded in count and unique. A duplicate is not a harmless repeat
//! — a global page is funded by a deadline, so scanning one session twice
//! spends budget the other selected sessions never get, and the page comes back
//! looking complete.
//!
//! Every bound is read from `terminal_search`; nothing here restates a number.

use std::collections::BTreeSet;
use std::ops::Deref;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::terminal_search::GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS;
use crate::wire::brand::SessionId;
use crate::wire::control::global_search::{TerminalSearchGridEpoch, TerminalSearchRow};
use crate::{ProtocolError, ProtocolResult};

/// One session inside a global search, bound to the grid epoch the caller holds
/// as authoritative. An absent `before_row` starts at the newest boundary, so
/// the first page of a session needs no cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalSearchSession {
    pub session_id: SessionId,
    pub grid_epoch: TerminalSearchGridEpoch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_row: Option<TerminalSearchRow>,
}

/// The selected sessions of one global search, bounded in count and unique by
/// session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct GlobalSearchSessions(Vec<GlobalSearchSession>);

impl Deref for GlobalSearchSessions {
    type Target = [GlobalSearchSession];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<Vec<GlobalSearchSession>> for GlobalSearchSessions {
    type Error = ProtocolError;

    fn try_from(entries: Vec<GlobalSearchSession>) -> ProtocolResult<Self> {
        let count = entries.len();
        if count == 0 || count > GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS {
            return Err(ProtocolError::new(
                "search-scrollback-batch.sessions",
                format!(
                    "must name 1..={GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS} sessions, got {count}"
                ),
            ));
        }
        let mut seen: BTreeSet<&SessionId> = BTreeSet::new();
        for (index, entry) in entries.iter().enumerate() {
            if !seen.insert(&entry.session_id) {
                return Err(ProtocolError::new(
                    format!("search-scrollback-batch.sessions[{index}].session_id"),
                    "global search sessions must be unique",
                ));
            }
        }
        Ok(Self(entries))
    }
}

impl<'de> Deserialize<'de> for GlobalSearchSessions {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        GlobalSearchSessions::try_from(Vec::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// The sessions a cancel names, bounded in count and unique. A cancel that
/// repeated a session would stop the first copy and leave the search running
/// until its own deadline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct GlobalSearchSessionIds(Vec<SessionId>);

impl Deref for GlobalSearchSessionIds {
    type Target = [SessionId];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<Vec<SessionId>> for GlobalSearchSessionIds {
    type Error = ProtocolError;

    fn try_from(ids: Vec<SessionId>) -> ProtocolResult<Self> {
        let count = ids.len();
        if count == 0 || count > GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS {
            return Err(ProtocolError::new(
                "cancel-scrollback-search-batch.session_ids",
                format!(
                    "must name 1..={GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS} sessions, got {count}"
                ),
            ));
        }
        let mut seen: BTreeSet<&SessionId> = BTreeSet::new();
        for (index, id) in ids.iter().enumerate() {
            if !seen.insert(id) {
                return Err(ProtocolError::new(
                    format!("cancel-scrollback-search-batch.session_ids[{index}]"),
                    "global search session IDs must be unique",
                ));
            }
        }
        Ok(Self(ids))
    }
}

impl<'de> Deserialize<'de> for GlobalSearchSessionIds {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        GlobalSearchSessionIds::try_from(Vec::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{GlobalSearchSession, GlobalSearchSessionIds, GlobalSearchSessions};
    use crate::terminal_search::GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS;
    use crate::wire::brand::SessionId;
    use crate::wire::control::global_search::TerminalSearchGridEpoch;

    const SESSION: &str = "00000000-0000-4000-8000-000000000001";

    fn session_id(index: usize) -> SessionId {
        SessionId::try_from(format!("00000000-0000-4000-8000-{index:012}").as_str())
            .expect("fixture session id")
    }

    fn entry(id: SessionId) -> GlobalSearchSession {
        GlobalSearchSession {
            session_id: id,
            grid_epoch: TerminalSearchGridEpoch::try_from("epoch:1").expect("fixture epoch"),
            before_row: None,
        }
    }

    fn entries(count: usize) -> Vec<GlobalSearchSession> {
        (0..count).map(|index| entry(session_id(index))).collect()
    }

    fn ids(count: usize) -> Vec<SessionId> {
        (0..count).map(session_id).collect()
    }

    #[test]
    fn a_batch_names_between_one_and_the_session_cap_with_no_repeats() {
        assert!(GlobalSearchSessions::try_from(entries(1)).is_ok());
        assert!(GlobalSearchSessions::try_from(Vec::new()).is_err());
        let repeated = vec![entry(session_id(0)), entry(session_id(0))];
        assert!(GlobalSearchSessions::try_from(repeated).is_err());
        let at_cap = GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS;
        assert!(GlobalSearchSessions::try_from(entries(at_cap)).is_ok());
        assert!(GlobalSearchSessions::try_from(entries(at_cap + 1)).is_err());
    }

    #[test]
    fn a_cancel_names_between_one_and_the_session_cap_with_no_repeats() {
        assert!(GlobalSearchSessionIds::try_from(ids(2)).is_ok());
        assert!(GlobalSearchSessionIds::try_from(Vec::new()).is_err());
        let repeated = vec![
            SessionId::try_from(SESSION).expect("fixture id"),
            SessionId::try_from(SESSION).expect("fixture id"),
        ];
        assert!(GlobalSearchSessionIds::try_from(repeated).is_err());
        let at_cap = GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS;
        assert!(GlobalSearchSessionIds::try_from(ids(at_cap)).is_ok());
        assert!(GlobalSearchSessionIds::try_from(ids(at_cap + 1)).is_err());
    }

    #[test]
    fn a_session_entry_refuses_a_key_the_contract_does_not_define() {
        let unknown_key = json!({
            "session_id": SESSION,
            "grid_epoch": "epoch:1",
            "before_row": 10,
            "regex": false,
        });
        let error = serde_json::from_value::<GlobalSearchSession>(unknown_key)
            .expect_err("an unknown key on a session entry is refused");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn an_over_limit_cursor_is_refused_before_a_scanner_reads_it() {
        let negative = json!({
            "session_id": SESSION,
            "grid_epoch": "epoch:1",
            "before_row": -1,
        });
        assert!(serde_json::from_value::<GlobalSearchSession>(negative).is_err());
    }
}
