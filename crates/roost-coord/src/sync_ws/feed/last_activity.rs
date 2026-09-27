//! Coordinator-owned last-activity timestamps, and the throttle that decides
//! when one is worth a frame.
//!
//! Ported from `apps/coord/src/sync/last-activity-hub.ts`. The hub is the
//! coordinator's own retained state -- reached as
//! `core.services.feed.last_activity()` -- and it is the reason this adapter is
//! not a plain bus-to-frame function: a worker's activity observation arrives on
//! every byte of output, and fanning a frame out for each one is how a busy
//! terminal pushes every other domain off its socket.
//!
//! TWO CLOCKS, AND ONLY ONE OF THEM GATES. The value retained is the WORKER's
//! observation, because that is what a sidebar ages sessions by and worker clock
//! skew cannot make it go backwards. The throttle is measured against LOCAL
//! receipt time, so a worker whose clock runs fast is throttled exactly like one
//! whose clock runs slow. v2 states the same rule at `last-activity-hub.ts:10-12`.
//!
//! THE HUB IS NOT SEEDED FROM THE BUS. `last_activity_bus` is volatile and never
//! replayed, so the retained map is what a fresh subscriber is seeded from; a
//! page load without it would show every idle session as active until the next
//! observation arrived.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::{FirehoseFrame, LastActivityFrame};

use crate::events::bus::Subscription;
use crate::events::bus_domains::Buses;
use crate::events::bus_messages::{LastActivityUpdate, SessionBusMessage};
use crate::sync_ws::feed::{FeedFrame, as_f64};

/// How long one session may go without a fanned-out activity frame, in
/// milliseconds.
///
/// v2's `TERMINAL_METADATA_ACTIVITY_THROTTLE_MS`
/// (`packages/protocol/src/terminal-metadata.ts:9`). The value is shared with
/// the OSC title parser's module in v2; in v3 that module's home
/// (`roost-protocol::terminal_metadata`) does not exist yet, and when it lands
/// this constant is the one line that must be re-pointed at it rather than kept
/// beside it.
pub const LAST_ACTIVITY_THROTTLE_MS: i64 = 60_000;

/// One session's retained activity and the moment its last frame went out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    /// The newest observation received, whether or not it was fanned out.
    last_ts: i64,
    /// Local receipt time of the last publication.
    last_published_at_ms: i64,
}

/// Every session's last-activity value, retained by the coordinator.
#[derive(Clone)]
pub struct LastActivityHub {
    entries: Arc<Mutex<BTreeMap<String, Entry>>>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl std::fmt::Debug for LastActivityHub {
    /// The hub holds a clock, and a log line wants the retained depth rather
    /// than the clock's value.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LastActivityHub")
            .field("retained", &self.retained())
            .finish()
    }
}

impl Default for LastActivityHub {
    fn default() -> Self {
        Self::new()
    }
}

impl LastActivityHub {
    /// A hub over the real clock, retaining nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(Arc::new(crate::serve::now_ms))
    }

    /// A hub whose clock the caller supplies, so a test never waits for this
    /// one to tick.
    #[must_use]
    pub fn with_clock(now_ms: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        Self {
            entries: Arc::new(Mutex::new(BTreeMap::new())),
            now_ms,
        }
    }

    /// Accept one observation, and return the update to fan out -- or `None`
    /// when the throttle holds it back.
    ///
    /// The FIRST observation for a session always publishes: a session that has
    /// never reported has no value to be stale, and holding it would leave it
    /// looking idle for a minute after it started.
    pub fn observe(&self, session_id: &str, observed_at_ms: i64) -> Option<LastActivityUpdate> {
        let received_at_ms = (self.now_ms)();
        // A worker's stamp is a hint, not a fact about this coordinator, so an
        // unusable one falls back to local receipt rather than being dropped:
        // the sidebar's job is "when did this last move", and local receipt is
        // a true answer to that.
        let timestamp = if observed_at_ms >= 0 {
            observed_at_ms
        } else {
            received_at_ms
        };
        let mut entries = self.lock();
        let Some(entry) = entries.get_mut(session_id) else {
            entries.insert(
                session_id.to_owned(),
                Entry {
                    last_ts: timestamp,
                    last_published_at_ms: received_at_ms,
                },
            );
            return Some(LastActivityUpdate {
                session_id: session_id.to_owned(),
                ts_ms: timestamp,
            });
        };
        entry.last_ts = timestamp;
        if received_at_ms - entry.last_published_at_ms < LAST_ACTIVITY_THROTTLE_MS {
            return None;
        }
        entry.last_published_at_ms = received_at_ms;
        Some(LastActivityUpdate {
            session_id: session_id.to_owned(),
            ts_ms: timestamp,
        })
    }

    /// Accept one observation and publish it if the throttle allows, returning
    /// whether a frame went out.
    ///
    /// This is the hub's whole job in one call, and it is the call a worker
    /// activity observation makes.
    pub fn observe_and_publish(
        &self,
        buses: &Buses,
        session_id: &str,
        observed_at_ms: i64,
    ) -> bool {
        let Some(update) = self.observe(session_id, observed_at_ms) else {
            return false;
        };
        buses.last_activity_bus.publish(update);
        tracing::debug!(
            event = "sync.last_activity.published",
            session_id,
            "fanned out a session activity observation"
        );
        true
    }

    /// Every retained value, for a fresh subscriber's seed.
    #[must_use]
    pub fn snapshot(&self) -> Vec<LastActivityUpdate> {
        self.lock()
            .iter()
            .map(|(session_id, entry)| LastActivityUpdate {
                session_id: session_id.clone(),
                ts_ms: entry.last_ts,
            })
            .collect()
    }

    /// Drop a closed session's retained value, so a session id reused later
    /// does not inherit the previous one's activity.
    pub fn release(&self, session_id: &str) {
        if self.lock().remove(session_id).is_some() {
            tracing::debug!(
                event = "sync.last_activity.released",
                session_id,
                "a closed session released its retained activity"
            );
        }
    }

    /// How many sessions are retained, for a log line or a test.
    #[must_use]
    pub fn retained(&self) -> usize {
        self.lock().len()
    }

    /// Release every closed session for as long as this handle lives.
    ///
    /// v2's `startLastActivityHub`, and the ONE bus subscription in this
    /// directory: it exists because the retained map is this hub's own state,
    /// not because the feed is fanning anything out. The socket shell or `serve`
    /// holds the returned handle; dropping it ends the subscription.
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

/// One last-activity observation as its frame.
pub fn last_activity_frame(update: &LastActivityUpdate) -> FeedFrame {
    FeedFrame::of(FirehoseFrame {
        frame: Some(Frame::LastActivity(Box::new(LastActivityFrame {
            session_id: update.session_id.clone(),
            ts_ms: as_f64(update.ts_ms),
            ..LastActivityFrame::default()
        }))),
        ..FirehoseFrame::default()
    })
}
