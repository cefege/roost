//! The decisions every `AgentStore` makes the same way: which run intent a
//! command appends, and whether a run lease may change hands. The database and
//! in-memory stores call these inside their own transaction or lock.

use crate::records::Entry;
use crate::traits::IntentChange;

/// The intent row a command appends. `last_before` is the journal's last
/// sequence before the command; `last_appended` its own last entry, if any.
#[must_use]
pub fn intent_entry(
    intent: IntentChange,
    last_before: u64,
    last_appended: Option<u64>,
) -> Option<Entry> {
    match intent {
        IntentChange::None => None,
        IntentChange::Activate => Some(Entry::RunIntent {
            active: true,
            through_seq: last_appended.unwrap_or(last_before),
            aborted: false,
            yielded: false,
        }),
        IntentChange::Abort => Some(Entry::RunIntent {
            active: false,
            through_seq: last_appended.unwrap_or(last_before),
            aborted: true,
            yielded: false,
        }),
    }
}

/// A stored lease row: owner, epoch and expiry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseRow {
    pub owner: String,
    pub epoch: u64,
    pub expires_ms: i64,
}

/// The epoch `owner` takes the lease at, or `None` while another owner's
/// lease is live. The same owner re-acquiring bumps the epoch, so whatever an
/// earlier incarnation of it still has in flight is fenced off.
#[must_use]
pub fn next_lease_epoch(current: Option<&LeaseRow>, owner: &str, now_ms: i64) -> Option<u64> {
    match current {
        None => Some(1),
        Some(row) if row.owner == owner || row.expires_ms <= now_ms => {
            Some(row.epoch.saturating_add(1))
        }
        Some(_) => None,
    }
}

/// The expiry a lease taken or renewed at `now_ms` carries.
#[must_use]
pub fn lease_expiry(now_ms: i64, ttl_ms: u64) -> i64 {
    now_ms.saturating_add(i64::try_from(ttl_ms).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::{LeaseRow, next_lease_epoch};

    fn row(owner: &str, epoch: u64, expires_ms: i64) -> LeaseRow {
        LeaseRow {
            owner: owner.into(),
            epoch,
            expires_ms,
        }
    }

    #[test]
    fn a_live_foreign_lease_is_refused_and_an_expired_one_is_taken_at_the_next_epoch() {
        assert_eq!(next_lease_epoch(None, "a", 10), Some(1));
        assert_eq!(next_lease_epoch(Some(&row("b", 4, 11)), "a", 10), None);
        assert_eq!(next_lease_epoch(Some(&row("b", 4, 10)), "a", 10), Some(5));
        assert_eq!(next_lease_epoch(Some(&row("a", 4, 99)), "a", 10), Some(5));
    }
}
