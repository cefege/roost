//! Coordinator-owned retained terminal titles, deduplicated across spinner
//! animation and fanned out on the title bus. Ports `terminal/terminal-title-hub.ts`,
//! plus the negotiated path of `terminal-metadata-adapter.ts` and `worker-terminal-metadata-frame.ts`
//! (`live_frames::publish_metadata` observes here). Their legacy WBinary title parser and its
//! route-retirement subscription are NOT ported: every v3 peer negotiates `terminal_metadata_v1`
//! and the coordinator refuses legacy Binary metadata. The Sync seed reads the snapshot.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::events::bus::Subscription;
use crate::events::bus_domains::Buses;
use crate::events::bus_messages::{SessionBusMessage, SessionTitleUpdate};

/// The longest title retained, in UTF-16 code units as v2 counts
/// (`TERMINAL_TITLE_MAX_LENGTH`, `packages/protocol/src/terminal-metadata.ts:8`).
pub const TERMINAL_TITLE_MAX_LENGTH: usize = 256;

/// One session's retained title and the key its repeats are compared by.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    title: String,
    dedup_key: String,
}

/// Every live session's last displayed title.
#[derive(Debug, Default)]
pub struct TerminalTitleHub {
    entries: Mutex<BTreeMap<String, Entry>>,
}

impl TerminalTitleHub {
    /// A hub that has seen no title.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept one semantic title observation; `true` when it was published.
    ///
    /// A repeat whose only difference is a braille spinner glyph is swallowed:
    /// an agent animates its title several times a second, and one frame per
    /// glyph would push every other domain off a busy socket.
    pub fn observe_title(&self, buses: &Buses, session_id: &str, raw_title: &str) -> bool {
        let (title, dedup_key) = normalize_terminal_title(raw_title);
        {
            let mut entries = self.lock();
            if entries
                .get(session_id)
                .is_some_and(|entry| entry.dedup_key == dedup_key)
            {
                return false;
            }
            entries.insert(
                session_id.to_owned(),
                Entry {
                    title: title.clone(),
                    dedup_key,
                },
            );
        }
        // Published with the map released: a title-bus listener is a Sync
        // socket, and nothing it does may wait on this hub.
        buses.title_bus.publish(SessionTitleUpdate {
            session_id: session_id.to_owned(),
            title,
        });
        tracing::debug!(
            event = "terminal_title.change",
            session_id,
            "a terminal title changed"
        );
        true
    }

    /// The current title of every session that has one, for a fresh Sync
    /// subscriber: the title bus is publish-on-change and never backfilled.
    #[must_use]
    pub fn title_snapshot(&self) -> Vec<SessionTitleUpdate> {
        self.lock()
            .iter()
            .map(|(session_id, entry)| SessionTitleUpdate {
                session_id: session_id.clone(),
                title: entry.title.clone(),
            })
            .collect()
    }

    /// Forget a closed session's title, so a reused id starts untitled.
    pub fn release(&self, session_id: &str) {
        if self.lock().remove(session_id).is_some() {
            tracing::debug!(
                event = "terminal_title.released",
                session_id,
                "a closed session released its retained title"
            );
        }
    }

    /// Release every closed session for as long as the returned handle lives.
    ///
    /// v2's `startTerminalTitleHub`; `serve` holds the handle for the process.
    pub fn subscribe_session_close(
        self: &Arc<Self>,
        buses: &Buses,
    ) -> Subscription<SessionBusMessage> {
        let hub = Arc::clone(self);
        buses.session_bus.subscribe(move |message| {
            if message.event.kind_name() != "closed" {
                return;
            }
            if let Some(session_id) = message.event.session_id() {
                hub.release(session_id.as_str());
            }
        })
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A title with its control characters removed and its length capped, and the
/// key a repeat is compared by: the same title with every braille spinner
/// glyph collapsed to one (`normalizeTerminalTitle`, `terminal-metadata.ts:25`).
///
/// The cap counts UTF-16 code units, as v2's `String.slice` does, and never
/// splits a character: a Rust string cannot hold half a surrogate pair.
#[must_use]
pub fn normalize_terminal_title(raw: &str) -> (String, String) {
    let mut units = 0;
    let title: String = raw
        .chars()
        .filter(|character| !matches!(*character, '\u{0}'..='\u{1f}' | '\u{7f}'))
        .take_while(|character| {
            units += character.len_utf16();
            units <= TERMINAL_TITLE_MAX_LENGTH
        })
        .collect();
    let dedup_key = title
        .chars()
        .map(|character| match character {
            '\u{2800}'..='\u{28ff}' => '\u{2800}',
            other => other,
        })
        .collect();
    (title, dedup_key)
}
