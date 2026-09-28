//! Stray PTY reaping, dead-birth detection, and channel-id allocation. Owned
//! by the worker.
//!
//! All pure, all driven by observed incidents, and all about the same
//! question: **which PTYs on this machine are nobody's responsibility any
//! more?**
//!
//! A keeper outlives the worker that spawned it, so a PTY whose session was
//! deleted can stay alive indefinitely. Nothing kills it, and the coordinator's
//! open rows drift steadily below the live PTY count — 12 rows against 88
//! processes was an observed state, not a hypothetical.
//!
//! Reaping is dangerous in the other direction: a channel that is briefly
//! untracked is a LIVE session, and killing it takes a user's terminal with
//! it. Every rule below is about telling those two apart, and every threshold
//! comes from a case where guessing wrong was observed.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// How often the reverse-reap sweep diffs the keeper's channels against the
/// worker's own sessions (v2 `STRAY_REAP_INTERVAL_MS`).
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// How many CONSECUTIVE sweeps a channel must read stray before it is reaped.
///
/// Two, not one. The worker's session table trails the keeper's spawn by a
/// beat — a just-spawned channel is briefly in the keeper and not yet tracked.
/// One grace interval covers that window; two strikes means a channel has been
/// untracked across two full minutes, which no live spawn is.
pub const STRAY_STRIKES: u32 = 2;

/// How long after a session record is deleted its channel may still emit
/// before those emissions count against the keeper's health.
///
/// A channel emits a few PTY bytes after the worker deleted its record — a
/// prompt epilogue, an exit message — because the keeper is a SEPARATE process
/// and in-flight frames arrive after the close. Those tail emissions are
/// benign; the bytes are correctly dropped. Counting them re-tripped the
/// degraded-keeper detector immediately after a reconcile, and the resulting
/// restart loop SIGTERMed every live PTY on the machine. Proven 2026-06-23.
///
/// Past this window it is a TRUE orphan — a degraded keeper driving a channel
/// nobody owns — and it does count.
pub const RECENTLY_CLOSED_TTL: Duration = Duration::from_millis(750);

/// How long after a spawn a child that produced nothing counts as stillborn.
pub const DEAD_BIRTH_LIFETIME: Duration = Duration::from_secs(2);

/// How many stillborn births within the window make the keeper degraded.
pub const DEAD_BIRTH_THRESHOLD: u32 = 3;

/// The window the degraded-keeper detectors count within.
pub const DEGRADED_WINDOW: Duration = Duration::from_secs(30);

/// How many emit-on-a-dead-channel observations make the keeper degraded.
///
/// Five, not one: a single mid-kill race is ordinary, and firing on it would
/// restart a healthy keeper. A SUSTAINED degraded keeper emits this many times
/// inside the window, so the threshold sits above the noise and below the
/// signal.
pub const DEGRADED_THRESHOLD: u32 = 5;

/// What a sweep decided about one channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Tracked by this worker. Leave it alone.
    Keep,
    /// Untracked, but not for long enough to be sure. Count a strike.
    Strike { channel_id: u16 },
    /// Untracked across enough sweeps to be certain. Kill it.
    Reap { channel_id: u16 },
}

/// The worker's view of which channels are strays: the decision half of v2
/// `session-lifecycle.ts` `reapStrayKeeperChannels`, whose `strayStrikes` map
/// this is. `session::stray_reap` lists the keeper and kills the reaps.
#[derive(Debug, Default)]
pub struct StrayTracker {
    /// Consecutive sweeps each channel has read stray. Dropped the moment it
    /// is seen as tracked, so a channel that flaps in and out of the worker's
    /// view never accumulates to a kill, and dropped on the reap, so a kill
    /// that did not land starts its two strikes over.
    strikes: HashMap<u16, u32>,
}

impl StrayTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run a sweep, deciding what to do with each channel the keeper reports.
    ///
    /// `tracked` is the worker's own authoritative set; `keeper_channels` is
    /// what the keeper says exists. A channel in both is ours, one only in the
    /// keeper is a candidate.
    pub fn sweep(&mut self, keeper_channels: &[u16], tracked: &HashSet<u16>) -> Vec<Verdict> {
        // A channel the keeper no longer reports exited on its own; its strike
        // is dead weight that would grow without bound.
        self.strikes
            .retain(|channel_id, _| keeper_channels.contains(channel_id));
        keeper_channels
            .iter()
            .map(|&channel_id| {
                if tracked.contains(&channel_id) {
                    self.strikes.remove(&channel_id);
                    return Verdict::Keep;
                }
                let strikes = self.strikes(channel_id).saturating_add(1);
                if strikes < STRAY_STRIKES {
                    self.strikes.insert(channel_id, strikes);
                    return Verdict::Strike { channel_id };
                }
                self.strikes.remove(&channel_id);
                Verdict::Reap { channel_id }
            })
            .collect()
    }

    /// How many strikes a channel has. For diagnostics, so an operator can see
    /// a channel approaching the threshold rather than discovering it dead.
    pub fn strikes(&self, channel_id: u16) -> u32 {
        self.strikes.get(&channel_id).copied().unwrap_or(0)
    }
}

/// A child that exited shortly after spawning without ever producing output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Birth {
    /// When the spawn returned.
    pub spawned_at: Instant,
    /// How many bytes the channel produced. ZERO is the discriminator.
    pub produced_bytes: u64,
}

/// Why a child that just exited is or is not stillborn.
///
/// The two "not stillborn" cases are named apart rather than collapsed: they
/// are different observations, and a log line that says only "no" tells an
/// operator nothing about which one applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stillborn {
    /// Exited fast having produced nothing: a degraded keeper born a dead PTY.
    Stillborn,
    /// Exited fast but produced output, so a real shell ran and finished.
    ProducedOutput,
    /// Lived long enough that its exit is ordinary, whatever it printed.
    LivedLongEnough,
}

impl Stillborn {
    /// Whether this birth should count toward the degraded-keeper threshold.
    pub fn is_stillborn(self) -> bool {
        self == Stillborn::Stillborn
    }
}

impl Birth {
    /// A spawn record.
    pub fn new(spawned_at: Instant) -> Self {
        Self {
            spawned_at,
            produced_bytes: 0,
        }
    }

    /// Note output. Only the zero/non-zero distinction matters, but the count
    /// is kept because a caller logging it wants the number.
    pub fn produced(&mut self, bytes: u64) {
        self.produced_bytes = self.produced_bytes.saturating_add(bytes);
    }

    /// Whether this birth is stillborn.
    pub fn verdict(&self, now: Instant) -> Stillborn {
        if now.saturating_duration_since(self.spawned_at) >= DEAD_BIRTH_LIFETIME {
            return Stillborn::LivedLongEnough;
        }
        if self.produced_bytes == 0 {
            Stillborn::Stillborn
        } else {
            // A real shell prints a prompt before it exits, so a fast `exit` is
            // NOT stillborn. "produced nothing" is the discriminator, and it
            // exists precisely so this case is not counted.
            Stillborn::ProducedOutput
        }
    }
}

/// The worker's channel-id allocator.
///
/// A keeper outlives the worker, so a fresh worker starts at one and collides
/// with channels the OLD keeper still holds. The counter must therefore be
/// advanced past the KEEPER's actual maximum, not merely past what the
/// coordinator lists — the coordinator does not know about orphaned PTYs from an
/// earlier keeper generation, and a colliding spawn is answered with
/// `channel_id in use` and the new terminal simply fails.
#[derive(Debug, Default)]
pub struct ChannelAllocator {
    next: u16,
    /// Whether the final id has been handed out. Tracked rather than inferred
    /// from an overflow, because the last id is usable EXACTLY once and a
    /// saturating counter would hand it out forever.
    exhausted: bool,
}

impl ChannelAllocator {
    pub fn new() -> Self {
        Self {
            next: 1,
            exhausted: false,
        }
    }

    /// The next id this worker will ask the keeper for.
    pub fn next(&self) -> u16 {
        self.next
    }

    /// Move the counter past every channel the keeper reports.
    ///
    /// Returns whether it moved, so a caller can log the reconcile rather than
    /// silently having done it.
    pub fn advance_past_keeper(&mut self, keeper_channels: &[u16]) -> bool {
        let Some(highest) = keeper_channels.iter().copied().max() else {
            return false;
        };
        if highest < self.next {
            return false;
        }
        match highest.checked_add(1) {
            Some(next) => self.next = next,
            // The keeper holds the final id, so there is nothing left to hand
            // out and saying so beats wrapping to zero.
            None => {
                self.next = u16::MAX;
                self.exhausted = true;
            }
        }
        true
    }

    /// Take the next id, or `None` when every id has been handed out.
    ///
    /// The last id is usable exactly once. Refusing to advance and therefore
    /// never handing it out would silently cost a channel, which is a bug that
    /// only shows up as one fewer terminal than expected.
    pub fn take(&mut self) -> Option<u16> {
        if self.exhausted {
            return None;
        }
        let current = self.next;
        match self.next.checked_add(1) {
            Some(next) => self.next = next,
            None => self.exhausted = true,
        }
        Some(current)
    }

    /// Whether every id has been handed out. For diagnostics; a caller that
    /// needs a channel should handle the refusal rather than assume.
    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }
}
