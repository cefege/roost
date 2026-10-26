//! Last-writer-wins merge of coordinator answers into the local store. One rule
//! serves both List and Set: the newer stamp wins, a local win is queued for the
//! next flush, and an equal stamp is a tie the coordinator resolves toward the
//! incoming write, so a local value still awaiting its push is kept and pushed.

use std::collections::BTreeSet;

use roost_protocol::client_state::is_synced_key;

use super::entry::ClientStateEntry;
use super::store::ClientStateStore;
use crate::platform::KeyValueStore;

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct MergeCounts {
    /// Remote values written into local storage.
    pub(crate) applied: usize,
    /// Local values that are newer and now queued for a push.
    pub(crate) queued: usize,
    /// Entries for keys outside the synced allowlist, dropped.
    pub(crate) ignored: usize,
}

/// Merge the entries a Set answered with. Set answers carry every requested key,
/// so no key outside them is considered.
pub(crate) fn merge_remote_entries(
    storage: &ClientStateStore,
    entries: &[ClientStateEntry],
) -> MergeCounts {
    let mut counts = MergeCounts::default();
    for entry in entries {
        if !is_synced_key(&entry.key) {
            counts.ignored += 1;
            continue;
        }
        let local_stamp = storage.stamp_of(&entry.key);
        if local_stamp.is_some_and(|local| local > entry.updated_ms) {
            storage.mark_dirty(&entry.key);
            counts.queued += 1;
        } else if local_stamp == Some(entry.updated_ms) {
            merge_equal_stamp(storage, entry, &mut counts);
        } else {
            storage.apply_remote(entry);
            counts.applied += 1;
        }
    }
    if counts.ignored > 0 {
        tracing::warn!(
            target: "client_state",
            ignored = counts.ignored,
            "client state answer carried non-synced keys; ignored"
        );
    }
    counts
}

/// The coordinator accepts a write at the stored stamp, so a tie goes to the
/// incoming value. A local value still awaiting its push is the write the
/// coordinator is about to receive, so it keeps its place and flushes.
fn merge_equal_stamp(
    storage: &ClientStateStore,
    entry: &ClientStateEntry,
    counts: &mut MergeCounts,
) {
    if storage.is_dirty(&entry.key) {
        counts.queued += 1;
    } else if storage.get(&entry.key).unwrap_or_default() != entry.value {
        storage.apply_remote(entry);
        counts.applied += 1;
    }
}

/// Merge a full List answer. A List carries every key the account has, so a
/// locally stamped key it omits was never pushed and is queued now.
pub(crate) fn merge_list_answer(
    storage: &ClientStateStore,
    entries: &[ClientStateEntry],
) -> MergeCounts {
    let mut counts = merge_remote_entries(storage, entries);
    let remote_keys: BTreeSet<&str> = entries.iter().map(|entry| entry.key.as_str()).collect();
    for key in storage.stamped_keys() {
        if !remote_keys.contains(key.as_str()) {
            storage.mark_dirty(&key);
            counts.queued += 1;
        }
    }
    counts
}
