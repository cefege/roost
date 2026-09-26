//! The unmapped-drop burst detector, which is a fact about one channel rather
//! than about the index.
//!
//! Split out of `route_index` because it is a different concern with its own
//! window and its own cap: it answers "is this channel losing bytes, and has
//! that been going on long enough to be real", while the index answers "which
//! session does this channel carry". Neither needs the other to be correct.

use std::collections::BTreeMap;

use roost_protocol::wire::{ChannelId, WorkerFp};

/// A single drop is the benign open-race; a sustained burst means real loss.
const UNMAPPED_DROP_THRESHOLD: u32 = 50;
const UNMAPPED_DROP_WINDOW_MS: i64 = 5_000;
/// Hard cap, so a pathological many-channel burst stays O(1) to record.
const UNMAPPED_DROP_MAX_ENTRIES: usize = 1_024;

#[derive(Debug, Clone, Copy)]
struct UnmappedDrop {
    count: u32,
    first_ms: i64,
}

/// Per-channel drop windows, in insertion order so the oldest is evictable
/// without a scan.
#[derive(Debug, Default)]
pub struct UnmappedDropDetector {
    entries: BTreeMap<(WorkerFp, ChannelId), UnmappedDrop>,
    last_sweep_ms: i64,
}

impl UnmappedDropDetector {
    /// A detector with no channels watched.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one frame that arrived on a channel nothing resolves to.
    ///
    /// A single drop is the benign open-race: the first PTY byte can beat the
    /// `opened` event that binds its channel, and a breadcrumb per drop would
    /// drown every other one. A SUSTAINED burst on one key is a mapping that
    /// never bound, which is real output loss, so it is raised.
    pub fn record(&mut self, worker_fp: &WorkerFp, channel_id: ChannelId, now_ms: i64) {
        self.sweep(now_ms);
        self.evict_to_room(now_ms);
        let key = (worker_fp.clone(), channel_id);
        let entry = self.entries.entry(key).or_insert(UnmappedDrop {
            count: 0,
            first_ms: now_ms,
        });
        if now_ms - entry.first_ms > UNMAPPED_DROP_WINDOW_MS {
            *entry = UnmappedDrop {
                count: 0,
                first_ms: now_ms,
            };
        }
        entry.count += 1;
        if entry.count > UNMAPPED_DROP_THRESHOLD {
            tracing::warn!(
                worker_fp = %worker_fp,
                channel_id = channel_id.as_u32(),
                drops = entry.count,
                window_ms = UNMAPPED_DROP_WINDOW_MS,
                "terminal bytes are dropping on an unbound channel"
            );
        }
    }

    /// A channel that just bound or published is no longer dropping, so a later
    /// transient drop starts a fresh window rather than inheriting this one.
    pub fn clear(&mut self, worker_fp: &WorkerFp, channel_id: ChannelId) {
        self.entries.remove(&(worker_fp.clone(), channel_id));
    }

    /// How many channels are currently being watched, for a log line.
    #[must_use]
    pub fn watched_channels(&self) -> usize {
        self.entries.len()
    }

    /// Retire windows nobody has touched for a whole window, so a channel that
    /// stops dropping does not pin its breadcrumb for the life of the process.
    fn sweep(&mut self, now_ms: i64) {
        if self.entries.is_empty() || now_ms - self.last_sweep_ms < UNMAPPED_DROP_WINDOW_MS {
            return;
        }
        self.last_sweep_ms = now_ms;
        let cutoff = now_ms - UNMAPPED_DROP_WINDOW_MS;
        self.entries.retain(|_, entry| entry.first_ms <= cutoff);
    }

    fn evict_to_room(&mut self, now_ms: i64) {
        while self.entries.len() >= UNMAPPED_DROP_MAX_ENTRIES {
            self.sweep(now_ms);
            if self.entries.len() < UNMAPPED_DROP_MAX_ENTRIES {
                return;
            }
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.first_ms)
                .map(|(key, _)| key.clone())
            else {
                return;
            };
            self.entries.remove(&oldest);
        }
    }
}
