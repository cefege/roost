//! Journal views every reader shares: the entries still in effect after
//! `Rewind` entries, so the transcript, the model history and usage totals
//! agree on what an undone turn removed.

use crate::records::Entry;

/// The entries in effect: each `Rewind { to_seq }` drops every earlier entry
/// with `seq >= to_seq`; the `Rewind` entries themselves are dropped too.
pub fn effective_entries(entries: &[(u64, Entry)]) -> Vec<(u64, Entry)> {
    let mut kept: Vec<(u64, Entry)> = Vec::with_capacity(entries.len());
    for (seq, entry) in entries {
        if let Entry::Rewind { to_seq } = entry {
            kept.retain(|(kept_seq, _)| kept_seq < to_seq);
            continue;
        }
        kept.push((*seq, entry.clone()));
    }
    kept
}

/// The seq of the latest user entry in effect: the current turn's checkpoint.
pub fn current_checkpoint(entries: &[(u64, Entry)]) -> u64 {
    effective_entries(entries)
        .iter()
        .rev()
        .find_map(|(seq, entry)| matches!(entry, Entry::User { .. }).then_some(*seq))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::effective_entries;
    use crate::records::Entry;

    fn user(text: &str) -> Entry {
        Entry::User { text: text.into() }
    }

    #[test]
    fn a_rewind_drops_its_turn_and_later_entries_but_not_later_appends() {
        let entries = vec![
            (1, user("a")),
            (2, user("b")),
            (3, user("c")),
            (4, Entry::Rewind { to_seq: 2 }),
            (5, user("d")),
        ];
        let kept: Vec<u64> = effective_entries(&entries)
            .iter()
            .map(|(seq, _)| *seq)
            .collect();
        assert_eq!(kept, vec![1, 5]);
    }
}
