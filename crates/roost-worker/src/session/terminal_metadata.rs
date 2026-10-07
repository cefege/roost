//! Worker-owned semantic terminal metadata: v2
//! `apps/worker/src/session/session-terminal-metadata.ts` (per-channel title and
//! activity facts, the negotiated flag, the fair 32-frame flush, replay after the
//! snapshot barrier) and `packages/protocol/src/terminal-metadata.ts`
//! (`TerminalTitleParser`). OSC 52 clipboard writes and long-command finishes
//! are events: sent once, never reasserted by a replay. `session::emit` observes
//! every chunk; the cadence flushes into the link's coalescing lane.

use std::collections::{HashMap, HashSet, VecDeque};

use roost_protocol::wire::brand::ChannelId;
use roost_term::core::CommandEvent;

use roost_protocol::wire::coord_worker::TerminalMetadata;
mod title_parser;
pub use title_parser::{TerminalTitleParser, TitleObservation, normalize_terminal_title};

/// v2 `TERMINAL_METADATA_DISPATCH_FRAME_BUDGET`.
pub const TERMINAL_METADATA_DISPATCH_FRAME_BUDGET: usize = 32;
/// v2 `TERMINAL_TITLE_CARRY_CAP` (characters).
pub const TERMINAL_TITLE_CARRY_CAP: usize = 1_024;
/// v2 `TERMINAL_TITLE_MAX_LENGTH` (UTF-16 code units, as v2 sliced).
pub const TERMINAL_TITLE_MAX_LENGTH: usize = 256;
/// v2 `TERMINAL_METADATA_ACTIVITY_THROTTLE_MS`.
pub const TERMINAL_METADATA_ACTIVITY_THROTTLE_MS: i64 = 60_000;
/// Minimum live command duration included in terminal metadata.
pub const COMMAND_FINISHED_MIN_DURATION_MS: i64 = 10_000;

/// What one send into the link did (v2 `TransportSendResult`, reduced to the
/// one distinction the flush acts on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataSend {
    Accepted,
    Dropped,
}

/// v2 `TerminalMetadataState`: one channel's retained facts, and the clipboard
/// write still waiting for its one send.
#[derive(Debug, Default, Clone)]
pub struct ChannelMetadata {
    parser: TerminalTitleParser,
    pub title: Option<String>,
    title_key: Option<String>,
    pub activity_ts_ms: Option<i64>,
    last_activity_published_at_ms: Option<i64>,
    pub clipboard: Option<String>,
    title_dirty: bool,
    activity_dirty: bool,
    pub clipboard_dirty: bool,
    command_started_at_ms: Option<i64>,
    command_finished: Option<(i32, u64)>,
    command_dirty: bool,
}

/// The semantic lane: every channel's facts, the ready ring, and the flag.
#[derive(Debug, Default)]
pub struct TerminalMetadataStage {
    negotiated: bool,
    channels: HashMap<ChannelId, ChannelMetadata>,
    ready: VecDeque<ChannelId>,
    ready_set: HashSet<ChannelId>,
    flush_scheduled: bool,
}

impl TerminalMetadataStage {
    pub fn negotiated(&self) -> bool {
        self.negotiated
    }

    /// A channel's retained facts, for diagnostics and tests.
    pub fn channel(&self, channel_id: ChannelId) -> Option<&ChannelMetadata> {
        self.channels.get(&channel_id)
    }

    /// Whether a flush is owed (v2's queued microtask / yielded timer).
    pub fn flush_due(&self) -> bool {
        self.flush_scheduled && self.negotiated && !self.ready.is_empty()
    }

    /// v2 `observeTerminalMetadata`: record one chunk without retaining it.
    /// Returns whether a flush became owed.
    pub fn observe(&mut self, channel_id: ChannelId, bytes: &[u8], now_ms: i64) -> bool {
        self.observe_live(channel_id, bytes, None, now_ms)
    }

    /// [`Self::observe`] for a live chunk, with the newest clipboard write it
    /// parsed. A write seen before the link negotiated metadata is dropped
    /// rather than held: by the time the link is up the operator has moved on.
    pub fn observe_live(
        &mut self,
        channel_id: ChannelId,
        bytes: &[u8],
        clipboard: Option<String>,
        now_ms: i64,
    ) -> bool {
        if bytes.is_empty() && clipboard.is_none() {
            return false;
        }
        let state = self.channels.entry(channel_id).or_default();
        let mut title_changed = false;
        if let Some(title) = state.parser.push(bytes)
            && state.title_key.as_deref() != Some(title.dedup_key.as_str())
        {
            state.title = Some(title.title);
            state.title_key = Some(title.dedup_key);
            state.title_dirty = true;
            title_changed = true;
        }
        let clipboard_changed = clipboard.is_some() && self.negotiated;
        if let Some(text) = clipboard.filter(|_| self.negotiated) {
            state.clipboard = Some(text);
            state.clipboard_dirty = true;
        }
        if !bytes.is_empty() {
            state.activity_ts_ms = Some(now_ms);
        }
        let activity_due = !bytes.is_empty()
            && state.last_activity_published_at_ms.is_none_or(|published| {
                now_ms - published >= TERMINAL_METADATA_ACTIVITY_THROTTLE_MS
            });
        if activity_due {
            state.activity_dirty = true;
        }
        if self.negotiated
            && (title_changed || activity_due || clipboard_changed)
            && !self.ready_set.contains(&channel_id)
        {
            return self.mark_ready(channel_id);
        }
        false
    }
    /// Observe command lifecycle events from the live terminal parser.
    pub fn observe_command_events(
        &mut self,
        channel_id: ChannelId,
        events: impl IntoIterator<Item = CommandEvent>,
        now_ms: i64,
    ) -> bool {
        let mut finished = false;
        let state = self.channels.entry(channel_id).or_default();
        for event in events {
            match event {
                CommandEvent::Started => state.command_started_at_ms = Some(now_ms),
                CommandEvent::Finished { exit_code } => {
                    let Some(started_at_ms) = state.command_started_at_ms.take() else {
                        continue;
                    };
                    let duration_ms = now_ms.saturating_sub(started_at_ms).max(0);
                    if self.negotiated && duration_ms >= COMMAND_FINISHED_MIN_DURATION_MS {
                        state.command_finished = Some((exit_code, duration_ms as u64));
                        state.command_dirty = true;
                        finished = true;
                    }
                }
            }
        }
        self.negotiated && finished && self.mark_ready(channel_id)
    }

    /// v2 `setTerminalMetadataNegotiated`. Returns whether a flush became owed.
    pub fn set_negotiated(&mut self, negotiated: bool) -> bool {
        if self.negotiated == negotiated {
            return false;
        }
        self.negotiated = negotiated;
        tracing::info!(negotiated, "terminal_metadata_mode");
        if !negotiated {
            self.ready.clear();
            self.ready_set.clear();
            return false;
        }
        self.reassert_retained_facts()
    }

    /// v2 `replayTerminalMetadata`: reassert retained facts once the reconnect
    /// snapshot is live. Returns whether a flush became owed.
    pub fn replay(&mut self) -> bool {
        self.negotiated && self.reassert_retained_facts()
    }

    fn reassert_retained_facts(&mut self) -> bool {
        let mut channels: Vec<ChannelId> = self.channels.keys().copied().collect();
        channels.sort_unstable();
        let mut owed = false;
        for channel_id in channels {
            if let Some(state) = self.channels.get_mut(&channel_id) {
                state.title_dirty |= state.title.is_some();
                state.activity_dirty |= state.activity_ts_ms.is_some();
            }
            owed |= self.mark_ready(channel_id);
        }
        owed
    }

    /// v2 `flushTerminalMetadata`: at most one budget of frames, round-robin by
    /// readiness. `live` is the session table's answer; `send` the link.
    pub fn flush(
        &mut self,
        live: &dyn Fn(ChannelId) -> bool,
        send: &mut dyn FnMut(TerminalMetadata) -> MetadataSend,
        now_ms: i64,
    ) {
        self.flush_scheduled = false;
        if !self.negotiated {
            return;
        }
        let mut frames = 0usize;
        while frames < TERMINAL_METADATA_DISPATCH_FRAME_BUDGET {
            let Some(channel_id) = self.ready.pop_front() else {
                return;
            };
            self.ready_set.remove(&channel_id);
            let Some(state) = self.channels.get(&channel_id).filter(|_| live(channel_id)) else {
                continue;
            };
            let title = state.title.clone();
            let activity = state.activity_ts_ms;
            let command_finished = state.command_dirty && state.command_finished.is_some();
            let command = state.command_finished;
            let title_changed = state.title_dirty && title.is_some();
            let clipboard_changed = state.clipboard_dirty && state.clipboard.is_some();
            let clipboard = state.clipboard.clone().unwrap_or_default();
            let activity_changed = state.activity_dirty && activity.is_some();
            if !title_changed && !activity_changed && !clipboard_changed && !command_finished {
                continue;
            }
            let result = send(TerminalMetadata {
                channel_id,
                title_changed,
                title: title.clone().unwrap_or_default(),
                activity_changed,
                activity_ts_ms: activity.map_or(0, |ts| u64::try_from(ts).unwrap_or(0)),
                clipboard_changed,
                clipboard: clipboard.clone(),
                command_finished,
                command_exit_code: command.filter(|_| command_finished).map(|(code, _)| code),
                command_duration_ms: command
                    .filter(|_| command_finished)
                    .map_or(0, |(_, duration)| duration),
            });
            frames += 1;
            if result == MetadataSend::Dropped {
                // Retried on the link's next writable notification.
                self.ready.push_back(channel_id);
                self.ready_set.insert(channel_id);
                tracing::debug!(%channel_id, "terminal metadata was refused by the link; it stays ready");
                return;
            }
            let Some(state) = self.channels.get_mut(&channel_id) else {
                continue;
            };
            if title_changed && state.title == title {
                state.title_dirty = false;
            }
            if activity_changed {
                state.last_activity_published_at_ms = Some(now_ms);
                if state.activity_ts_ms == activity {
                    state.activity_dirty = false;
                }
            }
            if clipboard_changed && state.clipboard.as_deref() == Some(clipboard.as_str()) {
                state.clipboard = None;
                state.clipboard_dirty = false;
            }
            if command_finished && state.command_finished == command {
                state.command_finished = None;
                state.command_dirty = false;
            }
            if (state.title_dirty
                || state.activity_dirty
                || state.clipboard_dirty
                || state.command_dirty)
                && !self.ready_set.contains(&channel_id)
            {
                self.ready.push_back(channel_id);
                self.ready_set.insert(channel_id);
            }
        }
        // A completed quantum yields to the link before the next one.
        self.flush_scheduled = !self.ready.is_empty();
    }

    /// v2 `disposeTerminalMetadataState`.
    pub fn forget_channel(&mut self, channel_id: ChannelId) {
        self.channels.remove(&channel_id);
        if self.ready_set.remove(&channel_id) {
            self.ready.retain(|queued| *queued != channel_id);
        }
    }

    /// Queue a channel and owe a flush. Returns whether the flush is newly owed.
    fn mark_ready(&mut self, channel_id: ChannelId) -> bool {
        if self.ready_set.insert(channel_id) {
            self.ready.push_back(channel_id);
        }
        let newly = !self.flush_scheduled;
        self.flush_scheduled = true;
        newly
    }
}
