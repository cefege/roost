//! The journal entry each queued `Steer` becomes once delivered, and whether
//! delivering it wakes an idle conversation. `runs.rs` drains the queue; this
//! module decides what each drained message is.

use crate::records::Entry;

use super::Steer;

impl Steer {
    pub(crate) fn into_entry(self) -> Entry {
        match self {
            Self::User(text) => Entry::User {
                text,
                loop_repeat: false,
            },
            Self::Advisory {
                item_id,
                severity,
                note,
            } => Entry::Advisory {
                item_id,
                severity,
                note,
                delivered: true,
            },
            Self::Persisted { entry, .. } => entry,
            Self::BackgroundJob(entry) => entry,
        }
    }

    /// Whether delivering this message starts a run on an idle conversation.
    pub(crate) fn wakes_idle_conversation(&self) -> bool {
        match self {
            Self::BackgroundJob(Entry::BackgroundJob { state, .. }) => state.wakes_conversation(),
            _ => true,
        }
    }
}
