//! Synchronized-output (DEC 2026) hold machinery: v2
//! `apps/worker/src/session/session-sync-output.ts`. The core has no
//! `synchronizedOutput()`, so [`SyncOutputScan`] tracks the mode beside it from
//! the byte stream; each hold carries `stream_fence::SyncOutputHold`'s two
//! ceilings (1 s wall, 2 000 pending rows) whose firing IS the expiry.
//! Called by `session::emit` and `session::cell_scheduler`; the cadence fires the wall.

use std::collections::HashMap;
use std::time::Instant;

use roost_protocol::wire::brand::ChannelId;
use roost_term::scrollback_origin;

use super::cell_gates::CellGate;
use super::emit::CellEmitter;
use super::types::SessionRecord;
use crate::stream_fence::{SYNC_OUTPUT_MAX_PENDING_ROWS, SYNC_OUTPUT_MAX_SILENT, SyncOutputHold};

/// The longest private-mode parameter list carried across a chunk boundary.
const SYNC_SCAN_CARRY_MAX: usize = 32;

/// What the streaming path must do with this chunk's frame (v2 `SyncOutputAction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutputAction {
    /// No synchronized frame is withholding anything: use the rate governor.
    Pass,
    /// Inside a synchronized frame with both ceilings intact: withhold.
    Hold,
    /// A boundary the browser is owed the withheld frame at.
    Flush,
}

/// DEC private mode 2026 state of one channel's byte stream, and its generation
/// (bumped on every closed→open transition, as the core's own counter was).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncOutputScan {
    pub open: bool,
    pub generation: u64,
    carry: Vec<u8>,
}

impl SyncOutputScan {
    /// Advance over one chunk. `CSI ? Pm h` / `CSI ? Pm l` naming 2026 anywhere in
    /// its parameter list opens / closes the synchronized frame.
    pub fn observe(&mut self, chunk: &[u8]) {
        let mut input = std::mem::take(&mut self.carry);
        input.extend_from_slice(chunk);
        let mut cursor = 0;
        while let Some(offset) = input[cursor..].iter().position(|byte| *byte == 0x1b) {
            let start = cursor + offset;
            match parse_private_mode(&input[start..]) {
                PrivateMode::Incomplete => {
                    let tail = &input[start..];
                    if tail.len() <= SYNC_SCAN_CARRY_MAX {
                        self.carry = tail.to_vec();
                    }
                    return;
                }
                PrivateMode::Other => cursor = start + 1,
                PrivateMode::Set { names_sync, len } => {
                    if names_sync && !self.open {
                        self.open = true;
                        self.generation += 1;
                    }
                    cursor = start + len;
                }
                PrivateMode::Reset { names_sync, len } => {
                    if names_sync {
                        self.open = false;
                    }
                    cursor = start + len;
                }
            }
        }
    }
}

enum PrivateMode {
    Incomplete,
    Other,
    Set { names_sync: bool, len: usize },
    Reset { names_sync: bool, len: usize },
}

/// Parse `ESC [ ? <digits ;>* (h|l)` at the head of `bytes`.
fn parse_private_mode(bytes: &[u8]) -> PrivateMode {
    let prefix = [0x1b, b'[', b'?'];
    for (index, expected) in prefix.iter().enumerate() {
        match bytes.get(index) {
            None => return PrivateMode::Incomplete,
            Some(byte) if byte != expected => return PrivateMode::Other,
            Some(_) => {}
        }
    }
    let mut names_sync = false;
    let mut param: u32 = 0;
    for (index, byte) in bytes.iter().enumerate().skip(prefix.len()) {
        match byte {
            b'0'..=b'9' => {
                param = param
                    .saturating_mul(10)
                    .saturating_add(u32::from(byte - b'0'))
            }
            b';' => {
                names_sync |= param == 2026;
                param = 0;
            }
            b'h' | b'l' => {
                names_sync |= param == 2026;
                let len = index + 1;
                return if *byte == b'h' {
                    PrivateMode::Set { names_sync, len }
                } else {
                    PrivateMode::Reset { names_sync, len }
                };
            }
            _ => return PrivateMode::Other,
        }
    }
    PrivateMode::Incomplete
}

/// One synchronized frame whose intermediate sends the emitter is withholding
/// (v2 `SyncOutputHold`).
#[derive(Debug)]
pub struct ChannelSyncHold {
    pub generation: u64,
    /// Monotonic scrollback total when the hold opened.
    pub sb_total_at_open: u64,
    pub ceilings: SyncOutputHold,
    pub tripped: bool,
}

/// Every channel's scan and open hold.
#[derive(Debug, Default)]
pub struct SyncOutputState {
    scans: HashMap<ChannelId, SyncOutputScan>,
    holds: HashMap<ChannelId, ChannelSyncHold>,
}

impl CellEmitter {
    /// Advance the channel's DEC 2026 scan over bytes entering its stream.
    pub(crate) fn observe_sync_output(&mut self, channel_id: ChannelId, chunk: &[u8]) {
        self.sync
            .scans
            .entry(channel_id)
            .or_default()
            .observe(chunk);
    }

    /// The channel's synchronized-output mode as the stream last declared it.
    pub fn synchronized_output(&self, channel_id: ChannelId) -> bool {
        self.sync
            .scans
            .get(&channel_id)
            .is_some_and(|scan| scan.open)
    }

    /// Whether a hold exists for the channel (v2 `syncOutputHolds.has`).
    pub fn sync_output_held(&self, channel_id: ChannelId) -> bool {
        self.sync.holds.contains_key(&channel_id)
    }

    /// Whether the channel's hold has tripped a ceiling.
    pub fn sync_output_tripped(&self, channel_id: ChannelId) -> Option<bool> {
        self.sync.holds.get(&channel_id).map(|hold| hold.tripped)
    }

    /// v2 `syncOutputAction`.
    pub(crate) fn sync_output_action(
        &mut self,
        record: &SessionRecord,
        now_ms: i64,
        now: Instant,
    ) -> SyncOutputAction {
        let channel_id = record.channel_id();
        let (open, generation) = self
            .sync
            .scans
            .get(&channel_id)
            .map_or((false, 0), |scan| (scan.open, scan.generation));
        if !open {
            let Some(hold) = self.sync.holds.get(&channel_id) else {
                return SyncOutputAction::Pass;
            };
            // A hold that withheld output owes one authoritative frame at the
            // boundary the application declared; a tripped one already flushed
            // unless its flush was a deferred full.
            let owed = !hold.tripped || self.sync_snapshot_owed(channel_id);
            self.release_sync_output_hold(channel_id);
            return if owed {
                SyncOutputAction::Flush
            } else {
                SyncOutputAction::Pass
            };
        }
        let pending_rows = self
            .sync
            .holds
            .get(&channel_id)
            .map(|hold| sync_pending_rows(record, hold.sb_total_at_open));
        if let (Some(hold), Some(rows)) = (self.sync.holds.get_mut(&channel_id), pending_rows) {
            if hold.generation == generation {
                if hold.tripped {
                    return SyncOutputAction::Pass;
                }
                hold.ceilings.measure_pending(rows);
                if !hold.ceilings.rows_exceeded() {
                    return SyncOutputAction::Hold;
                }
                self.trip_sync_output_hold(channel_id, "pending_rows", now_ms);
                return SyncOutputAction::Flush;
            }
            if !hold.tripped {
                // Closed and reopened with nothing emitted between: the new
                // generation INHERITS the running ceilings, or a `2026l 2026h`
                // loop would suppress forever one reset at a time.
                hold.generation = generation;
                return SyncOutputAction::Hold;
            }
            self.release_sync_output_hold(channel_id);
        }
        let mut ceilings = SyncOutputHold::new();
        ceilings.open(now);
        self.sync.holds.insert(
            channel_id,
            ChannelSyncHold {
                generation,
                sb_total_at_open: mono_scrollback_total(record),
                ceilings,
                tripped: false,
            },
        );
        tracing::debug!(%channel_id, generation, "a synchronized-output hold opened");
        SyncOutputAction::Hold
    }

    /// v2's armed wall timer firing: trip the hold and emit the withheld frame,
    /// forced when the withheld frame is an owed full.
    pub(crate) fn fire_sync_output_ceiling(
        &mut self,
        record: &mut SessionRecord,
        now_ms: i64,
        now: Instant,
    ) {
        let channel_id = record.channel_id();
        if !self
            .sync_output_deadline(channel_id)
            .is_some_and(|due| due <= now)
        {
            return;
        }
        self.trip_sync_output_hold(channel_id, "elapsed_ms", now_ms);
        let force_owed_full = self.sync_snapshot_owed(channel_id);
        self.emit_cell_frame_at(record, force_owed_full, now_ms, now);
    }

    /// An untripped hold's wall deadline.
    pub(crate) fn sync_output_deadline(&self, channel_id: ChannelId) -> Option<Instant> {
        self.sync
            .holds
            .get(&channel_id)
            .filter(|hold| !hold.tripped)
            .and_then(|hold| hold.ceilings.deadline())
    }

    /// Every untripped hold's wall deadline, for the cadence.
    pub(crate) fn sync_output_deadlines(&self) -> Vec<(ChannelId, Instant)> {
        self.sync
            .holds
            .iter()
            .filter(|(_, hold)| !hold.tripped)
            .filter_map(|(channel_id, hold)| hold.ceilings.deadline().map(|due| (*channel_id, due)))
            .collect()
    }

    /// v2 `mayRearmCellEmission`'s last clause: no hold, or a tripped one.
    pub(crate) fn sync_output_permits_rearm(&self, channel_id: ChannelId) -> bool {
        self.sync
            .holds
            .get(&channel_id)
            .is_none_or(|hold| hold.tripped)
    }

    /// v2 `releaseSyncOutputHold`: stop withholding and forget the generation.
    /// The resize transaction calls it when the core it is expressed in freezes
    /// or is replaced.
    pub fn release_sync_output_hold(&mut self, channel_id: ChannelId) {
        let Some(hold) = self.sync.holds.remove(&channel_id) else {
            return;
        };
        if self
            .gates
            .get(&channel_id)
            .is_some_and(|held| held.gate == CellGate::SyncOutput)
        {
            self.gates.remove(&channel_id);
        }
        tracing::debug!(%channel_id, generation = hold.generation, tripped = hold.tripped, "a synchronized-output hold was released");
    }

    /// Forget the channel's scan and hold (dispose).
    pub(crate) fn forget_sync_output(&mut self, channel_id: ChannelId) {
        self.release_sync_output_hold(channel_id);
        self.sync.scans.remove(&channel_id);
    }

    /// v2 `tripSyncOutputHold`: stop withholding this generation, and leave the
    /// trip legible in the suppression record until the application closes.
    fn trip_sync_output_hold(&mut self, channel_id: ChannelId, cap: &str, now_ms: i64) {
        let Some(hold) = self.sync.holds.get_mut(&channel_id) else {
            return;
        };
        hold.tripped = true;
        let generation = hold.generation;
        let suppression = self
            .gates
            .get_mut(&channel_id)
            .filter(|held| held.gate == CellGate::SyncOutput);
        let (age_ms, suppressed) = match suppression {
            Some(held) => {
                held.over_budget = true;
                (now_ms.saturating_sub(held.since_ms), held.suppressed)
            }
            None => (0, 0),
        };
        tracing::warn!(
            %channel_id,
            cap,
            generation,
            age_ms,
            suppressed_frames = suppressed,
            cap_ms = SYNC_OUTPUT_MAX_SILENT.as_millis() as u64,
            cap_rows = SYNC_OUTPUT_MAX_PENDING_ROWS,
            "terminal.sync_output_cap"
        );
    }
}

/// Roost's monotonic scrollback total: the eviction origin plus what the ring
/// still holds, so it keeps moving at saturation.
fn mono_scrollback_total(record: &SessionRecord) -> u64 {
    let core = record.terminal_core.as_ref();
    let origin = scrollback_origin(core, record.cell_emit.scrollback_origin).unwrap_or(0);
    origin + core.scrollback_count() as u64
}

/// Rows the browser is missing inside this frame: history appended since the
/// hold opened plus the viewport rows currently dirty.
fn sync_pending_rows(record: &SessionRecord, sb_total_at_open: u64) -> u64 {
    let core = record.terminal_core.as_ref();
    let dirty = (0..core.rows())
        .filter(|row| core.is_dirty_row(*row))
        .count() as u64;
    mono_scrollback_total(record).saturating_sub(sb_total_at_open) + dirty
}
