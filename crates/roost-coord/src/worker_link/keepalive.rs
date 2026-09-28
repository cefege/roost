//! The coordinator's application heartbeat on one worker link: one outstanding
//! ping at a time, and the pong deadline that closes a link whose peer stopped
//! answering.
//!
//! Ports `apps/coord/src/workers/worker-conn-keepalive.ts`. Owned by
//! `worker_link::link_session`, which asks when to wake and what is due. Pure
//! over a `tokio::time::Instant` it is handed, so a paused tokio clock drives
//! it in tests exactly as elapsed time drives it in production.
//!
//! ONLY THE EXACT PONG COUNTS. A ping carries its generation as `ts`, and a
//! pong for any other generation neither clears the deadline nor schedules the
//! next ping, so a worker replaying stale pongs cannot pass for a live one.

use std::time::Duration;

use tokio::time::Instant;

/// How long after a hello, or after an exact pong, the next ping is sent.
/// 30 s (`WORKER_PING_DELAY_MS`, `worker-conn-keepalive.ts:14`).
pub const WORKER_PING_DELAY: Duration = Duration::from_secs(30);

/// How long a ping may go unanswered before the link is closed. 90 s
/// (`WORKER_PONG_TIMEOUT_MS`, `worker-conn-keepalive.ts:15`).
pub const WORKER_PONG_TIMEOUT: Duration = Duration::from_secs(90);

/// What the schedule says is due at a wake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepaliveDue {
    /// Nothing yet.
    Nothing,
    /// Send `ping` carrying this generation as its `ts`.
    Ping { generation: u64 },
    /// The ping of this generation was never answered: close the link.
    PongTimeout { generation: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// No ping scheduled and none outstanding.
    Idle,
    /// The next ping goes out at this instant.
    PingAt(Instant),
    /// This generation is outstanding until its pong, or the deadline.
    AwaitingPong { generation: u64, deadline: Instant },
    /// The link is ending; nothing is scheduled again.
    Stopped,
}

/// One link's ping schedule.
#[derive(Debug, Clone)]
pub struct PingSchedule {
    phase: Phase,
    last_generation: u64,
}

impl Default for PingSchedule {
    fn default() -> Self {
        Self::new()
    }
}

impl PingSchedule {
    /// Nothing scheduled: the hello is what starts the first delay.
    #[must_use]
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle,
            last_generation: 0,
        }
    }

    /// Start the delay to the next ping, unless one is scheduled, outstanding,
    /// or the schedule stopped (v2 `scheduleNextPing`'s guard).
    pub fn schedule_next_ping(&mut self, now: Instant) {
        if self.phase == Phase::Idle {
            self.phase = Phase::PingAt(now + WORKER_PING_DELAY);
        }
    }

    /// When the read loop must wake for this schedule, if at all.
    #[must_use]
    pub fn next_wake(&self) -> Option<Instant> {
        match self.phase {
            Phase::PingAt(at) => Some(at),
            Phase::AwaitingPong { deadline, .. } => Some(deadline),
            Phase::Idle | Phase::Stopped => None,
        }
    }

    /// What is due at `now`, advancing the schedule past it.
    ///
    /// The pong deadline starts when the ping is SENT, not when it was
    /// scheduled, which is v2's order: the deadline timer is armed inside the
    /// ping timer's callback.
    pub fn fire(&mut self, now: Instant) -> KeepaliveDue {
        match self.phase {
            Phase::PingAt(at) if now >= at => {
                self.last_generation += 1;
                let generation = self.last_generation;
                self.phase = Phase::AwaitingPong {
                    generation,
                    deadline: now + WORKER_PONG_TIMEOUT,
                };
                KeepaliveDue::Ping { generation }
            }
            Phase::AwaitingPong {
                generation,
                deadline,
            } if now >= deadline => {
                self.phase = Phase::Stopped;
                KeepaliveDue::PongTimeout { generation }
            }
            _ => KeepaliveDue::Nothing,
        }
    }

    /// A pong arrived carrying `ts`. Only the outstanding generation clears the
    /// deadline and starts a fresh delay; reports whether this one did.
    pub fn accept_pong(&mut self, ts: i64, now: Instant) -> bool {
        let Phase::AwaitingPong { generation, .. } = self.phase else {
            return false;
        };
        if i64::try_from(generation) != Ok(ts) {
            return false;
        }
        self.phase = Phase::Idle;
        self.schedule_next_ping(now);
        true
    }

    /// Stop for good: a fenced generation neither pings nor times out
    /// (v2 `stop`, reached from `revoke` and `close`).
    pub fn stop(&mut self) {
        self.phase = Phase::Stopped;
    }
}
