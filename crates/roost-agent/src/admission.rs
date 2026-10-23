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
        IntentChange::None
        | IntentChange::RejectIfBusy
        | IntentChange::FollowUp
        | IntentChange::Withdraw
        | IntentChange::Complete => None,
        IntentChange::Activate | IntentChange::RestartIfCurrent => Some(Entry::RunIntent {
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

/// Materializes pending follow-ups exactly once at a normal run boundary.
#[must_use]
pub fn queued_inputs_to_activate(entries: &[(u64, Entry)]) -> Vec<Entry> {
    use std::collections::{HashMap, HashSet};

    use crate::records::QueuedInputState;

    let mut latest: HashMap<String, (u64, String, QueuedInputState)> = HashMap::new();
    let mut withdrawn = HashSet::new();
    let mut activated_users = HashSet::new();
    for (sequence, entry) in entries {
        match entry {
            Entry::QueuedInput {
                submission_id,
                text,
                state,
            } => {
                latest.insert(submission_id.clone(), (*sequence, text.clone(), *state));
                withdrawn.remove(submission_id);
            }
            Entry::QueuedInputWithdrawn { submission_id } => {
                withdrawn.insert(submission_id.clone());
            }
            Entry::User {
                item_id: Some(item_id),
                ..
            } => {
                activated_users.insert(item_id.clone());
            }
            _ => {}
        }
    }
    let mut pending: Vec<_> = latest
        .into_iter()
        .filter_map(|(submission_id, (sequence, text, state))| {
            (state == QueuedInputState::Pending
                && !withdrawn.contains(&submission_id)
                && !activated_users.contains(&submission_id))
            .then_some((sequence, submission_id, text))
        })
        .collect();
    pending.sort_by_key(|(sequence, _, _)| *sequence);
    pending
        .into_iter()
        .flat_map(|(_, submission_id, text)| {
            [
                Entry::QueuedInput {
                    submission_id: submission_id.clone(),
                    text: text.clone(),
                    state: QueuedInputState::Activated,
                },
                Entry::User {
                    text,
                    loop_repeat: false,
                    item_id: Some(submission_id),
                },
            ]
        })
        .collect()
}
/// Converts one submitted queue item into the active transcript/history pair.
#[must_use]
pub fn activate_followup(entry: Entry) -> Option<[Entry; 2]> {
    let Entry::QueuedInput {
        submission_id,
        text,
        ..
    } = entry
    else {
        return None;
    };
    Some([
        Entry::QueuedInput {
            submission_id: submission_id.clone(),
            text: text.clone(),
            state: crate::records::QueuedInputState::Activated,
        },
        Entry::User {
            text,
            loop_repeat: false,
            item_id: Some(submission_id),
        },
    ])
}

/// Whether a stable submission id still names an unconsumed queued input.
#[must_use]
pub fn submission_is_withdrawable(entries: &[(u64, Entry)], submission_id: &str) -> bool {
    let entries = crate::journal::effective_entries(entries);
    let mut latest_state = None;
    let mut user_sequence = None;
    let mut consumed_through = 0;
    let mut withdrawn = false;
    for (sequence, entry) in &entries {
        match entry {
            Entry::QueuedInput {
                submission_id: id,
                state,
                ..
            } if id == submission_id => {
                latest_state = Some(*state);
                withdrawn = false;
            }
            Entry::QueuedInputWithdrawn { submission_id: id } if id == submission_id => {
                withdrawn = true;
            }
            Entry::User {
                item_id: Some(id), ..
            } if id == submission_id => user_sequence = Some(*sequence),
            Entry::InputConsumed { through_seq } => {
                consumed_through = consumed_through.max(*through_seq)
            }
            Entry::Assistant {
                metadata: Some(metadata),
                ..
            } => consumed_through = consumed_through.max(metadata.input_through_seq),
            _ => {}
        }
    }
    if withdrawn {
        return false;
    }
    if latest_state == Some(crate::records::QueuedInputState::Pending) {
        return true;
    }
    user_sequence.is_some_and(|sequence| sequence > consumed_through)
}

/// A conditional restart cannot overtake Stop or work admitted after its boundary.
#[must_use]
pub fn restart_boundary_matches(entries: &[(u64, Entry)], expected: Option<u64>) -> bool {
    entries
        .iter()
        .rev()
        .find_map(|(sequence, entry)| match entry {
            Entry::RunIntent { aborted, .. } => Some(expected == Some(*sequence) && !aborted),
            _ => None,
        })
        .unwrap_or(false)
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
