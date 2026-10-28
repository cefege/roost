//! Journal views every reader shares: the entries still in effect after
//! `Rewind` entries, so the transcript, the model history and usage totals
//! agree on what an undone turn removed; and the order the model actually
//! read user messages in, which the transcript and the history both show.

use crate::records::Entry;

/// The entries in effect: each `Rewind { to_seq }` drops every earlier entry
/// with `seq >= to_seq`; the `Rewind` entries themselves are dropped too.
/// Lifecycle and loop snapshots remain durable control state across rewinds.
pub fn effective_entries(entries: &[(u64, Entry)]) -> Vec<(u64, Entry)> {
    let mut kept: Vec<(u64, Entry)> = Vec::with_capacity(entries.len());
    for (seq, entry) in entries {
        if let Entry::Rewind { to_seq } = entry {
            kept.retain(|(kept_seq, kept_entry)| {
                kept_seq < to_seq
                    || matches!(
                        kept_entry,
                        Entry::RunIntent { .. }
                            | Entry::LoopState { .. }
                            | Entry::Goal { .. }
                            | Entry::QueuedInput { .. }
                            | Entry::QueuedInputWithdrawn { .. }
                            | Entry::InputConsumed { .. }
                    )
            });
            continue;
        }
        kept.push((*seq, entry.clone()));
    }
    let withdrawn: std::collections::HashSet<String> = kept
        .iter()
        .filter_map(|(_, entry)| match entry {
            Entry::QueuedInputWithdrawn { submission_id } => Some(submission_id.clone()),
            _ => None,
        })
        .collect();
    kept.retain(|(_, entry)| {
        !matches!(
            entry,
            Entry::User {
                item_id: Some(item_id),
                ..
            } if withdrawn.contains(item_id)
        )
    });
    kept
}

/// The entries with each user message moved to where the model first read
/// it: just before the first assistant entry whose request included it.
///
/// A message sent while a model call is in flight is journaled when it is
/// accepted, ahead of that call's reply, but the call never saw it; the
/// reply belongs before it, or the next request reads the old reply as the
/// answer to the new message. A message no request has read yet goes last.
/// An assistant entry without a recorded input bound read everything before
/// it, which keeps journals written before the bound existed in their order.
pub fn delivery_order(entries: Vec<(u64, Entry)>) -> Vec<(u64, Entry)> {
    let mut ordered = Vec::with_capacity(entries.len());
    let mut unread: Vec<(u64, Entry)> = Vec::new();
    for (seq, entry) in entries {
        match &entry {
            Entry::User { .. } | Entry::BackgroundJob { .. } | Entry::PeerMessage { .. } => {
                unread.push((seq, entry));
            }
            Entry::Assistant { metadata, .. } => {
                let read_through = metadata
                    .as_ref()
                    .map(|metadata| metadata.input_through_seq)
                    .filter(|through| *through > 0)
                    .unwrap_or_else(|| seq.saturating_sub(1));
                let (read, later): (Vec<_>, Vec<_>) = std::mem::take(&mut unread)
                    .into_iter()
                    .partition(|(user_seq, _)| *user_seq <= read_through);
                ordered.extend(read);
                unread = later;
                ordered.push((seq, entry));
            }
            _ => ordered.push((seq, entry)),
        }
    }
    ordered.extend(unread);
    ordered
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
    use super::{delivery_order, effective_entries};
    use crate::records::{AssistantMetadata, Entry};

    fn user(text: &str) -> Entry {
        Entry::User {
            text: text.into(),
            attachments: Vec::new(),
            loop_repeat: false,
            item_id: None,
        }
    }

    fn assistant(read_through: Option<u64>) -> Entry {
        Entry::Assistant {
            item_id: "a".into(),
            blocks: Vec::new(),
            usage: roost_llm::Usage::default(),
            model: None,
            metadata: read_through.map(|input_through_seq| AssistantMetadata {
                completed: true,
                stop_reason: None,
                input_through_seq,
                continued: false,
                yield_reminders: 0,
                goal_id: None,
            }),
        }
    }

    fn seqs(entries: &[(u64, Entry)]) -> Vec<u64> {
        entries.iter().map(|(seq, _)| *seq).collect()
    }

    #[test]
    fn a_message_sent_during_a_call_follows_that_calls_reply() {
        // 1 asks; the call reading through 1 streams while 2 arrives; its
        // reply lands at 3; the next call reads through 2 and replies at 4.
        let entries = vec![
            (1, user("first")),
            (2, user("sent while streaming")),
            (3, assistant(Some(1))),
            (4, assistant(Some(2))),
        ];
        assert_eq!(seqs(&delivery_order(entries)), vec![1, 3, 2, 4]);
    }

    #[test]
    fn an_unread_message_goes_last_and_old_journals_keep_their_order() {
        let entries = vec![
            (1, user("a")),
            (2, assistant(None)),
            (3, user("b")),
            (4, assistant(Some(1))),
        ];
        assert_eq!(seqs(&delivery_order(entries)), vec![1, 2, 4, 3]);
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
