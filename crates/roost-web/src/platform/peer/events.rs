//! What the browser's WebRTC stack reported, as values rather than as calls
//! back into the host.
//!
//! Owned by `platform::peer`. A browser delivers ICE and data-channel events by
//! invoking a function, and the only safe thing for such a function to do is put
//! the fact somewhere: it may fire while the pump is already borrowed, and a
//! callback that reached for the store from inside one would be a re-entrant
//! borrow on a single-threaded event loop. So every callback here pushes a
//! [`PeerEvent`] and rings the host's notify, and the host drains the sink on
//! the next task — v2 handled each data-channel message as it arrived, and a
//! frame left for the host's slow tick reaches the screen up to a tick late.
//!
//! Target-independent on purpose: the queue, its bound and the settle order are
//! decided by native tests, and only the JS closures that push into it are
//! `wasm32`.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use roost_client_core::client::carriers::{CandidateType, PeerLane};

/// What the browser's own stats report says about one peer's selected pair: the
/// kind of address it reached the far end at, which only the browser knows.
/// Liveness and round trip are the transport probe's (`pump::peer_lane::liveness`):
/// a selected pair outlives the worker process behind it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PeerMeasurement {
    /// Which kind of address the selected pair reached the far end at, `None`
    /// when no pair is selected.
    pub candidate_type: CandidateType,
}

/// How many events one document may hold unread.
///
/// A browser can deliver a channel's bytes far faster than the host drains them,
/// so the queue needs a bound: an unbounded one is a peer that fills this
/// document's memory before any fault is noticed. Overflow drops the NEWEST
/// events and retires the peer that produced them — a carrier whose frames were
/// discarded cannot be treated as a carrier that carried them.
pub const PEER_EVENT_BACKLOG: usize = 4_096;

/// One thing the browser told this document about one open peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerEvent {
    /// ICE gathering finished. The offer is read on the host's own tick, which
    /// is what keeps a browser callback from deciding when a negotiation starts.
    Gathered {
        /// Which attempt.
        attempt_id: u64,
    },
    /// The browser gathered a server-reflexive candidate. The offer is then read
    /// after a short settle rather than when every interface has given up.
    ReflexiveCandidate {
        /// Which attempt.
        attempt_id: u64,
    },
    /// One lane's data channel became writable.
    LaneOpen {
        /// Which attempt.
        attempt_id: u64,
        /// Which lane.
        lane: PeerLane,
    },
    /// One lane closed or errored. `reason` is for the host's own log: the peer
    /// is gone either way, and the message is not part of the wire contract.
    LaneFailed {
        /// Which attempt.
        attempt_id: u64,
        /// Which lane.
        lane: PeerLane,
        /// What the browser said.
        reason: String,
    },
    /// Bytes arrived on a lane, whole: a data channel delivers one message at a
    /// time, so fragmentation on this side is the carrier's, not the browser's.
    Bytes {
        /// Which attempt.
        attempt_id: u64,
        /// Which lane.
        lane: PeerLane,
        /// The message.
        bytes: Vec<u8>,
    },
    /// ICE could not connect, or the connection is gone.
    IceFailed {
        /// Which attempt.
        attempt_id: u64,
        /// What the browser said.
        reason: String,
    },

    /// The browser's stats report settled for one peer's selected pair.
    ///
    /// A MEASUREMENT and not an event about the attempt: it moves no phase and
    /// it decides nothing, it is only what the route diagnostic publishes. It
    /// travels through the same bounded sink so a report that settles after the
    /// peer closed is discarded by attempt id rather than applied to whatever
    /// took its place.
    Measured {
        /// Which attempt.
        attempt_id: u64,
        /// What the browser measured.
        measurement: PeerMeasurement,
    },
}

/// Named rather than counted quietly: an event the host never saw is exactly the
/// silence this file exists to end, so the retire that follows it is the point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overflowed {
    /// How many events were dropped.
    pub dropped: usize,
    /// The attempts that produced them, so each is retired rather than left
    /// holding a carrier whose frames are incomplete.
    pub attempt_ids: Vec<u64>,
}

/// What the host asked to be rung with after each recorded event.
type SinkNotify = Rc<dyn Fn()>;

/// Where the browser's callbacks put what they saw.
#[derive(Clone, Default)]
pub struct PeerEventSink {
    queue: Rc<RefCell<Vec<PeerEvent>>>,
    dropped: Rc<RefCell<Vec<u64>>>,
    /// Shared by every clone, so a callback installed before the host set it
    /// still rings.
    notify: Rc<RefCell<Option<SinkNotify>>>,
}

impl fmt::Debug for PeerEventSink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PeerEventSink")
            .field(
                "queued",
                &self.queue.try_borrow().map(|queue| queue.len()).ok(),
            )
            .field(
                "dropped",
                &self.dropped.try_borrow().map(|ids| ids.len()).ok(),
            )
            .field(
                "notifies",
                &self.notify.try_borrow().map(|notify| notify.is_some()).ok(),
            )
            .finish()
    }
}

impl PeerEventSink {
    /// A sink with nothing in it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ring `notify` after every recorded event, dropped ones included, so the
    /// host drains — and retires an overflowed attempt — without waiting for
    /// its tick.
    pub fn notify_on_record(&self, notify: SinkNotify) {
        *self.notify.borrow_mut() = Some(notify);
    }

    /// Record one fact, then ring the host. Never allocates after the bound: a
    /// full queue drops the event and remembers which attempt it belonged to.
    pub fn record(&self, event: PeerEvent) {
        self.enqueue(event);
        // Cloned out first: the host's notify may read this sink.
        let notify = self.notify.borrow().clone();
        if let Some(notify) = notify {
            notify();
        }
    }

    fn enqueue(&self, event: PeerEvent) {
        let attempt_id = event.attempt_id();
        let mut queue = self.queue.borrow_mut();
        if queue.len() >= PEER_EVENT_BACKLOG {
            let mut dropped = self.dropped.borrow_mut();
            if !dropped.contains(&attempt_id) {
                dropped.push(attempt_id);
            }
            return;
        }
        queue.push(event);
    }

    /// Take everything recorded so far, in arrival order.
    pub fn drain(&self) -> Vec<PeerEvent> {
        let mut queue = self.queue.borrow_mut();
        std::mem::take(&mut *queue)
    }

    /// The attempts whose events were dropped, and forget them.
    pub fn take_overflowed(&self) -> Overflowed {
        let mut dropped = self.dropped.borrow_mut();
        let attempt_ids = std::mem::take(&mut *dropped);
        Overflowed {
            dropped: attempt_ids.len(),
            attempt_ids,
        }
    }
}

impl PeerEvent {
    /// The attempt this fact is about, which is what a retire is named by.
    pub fn attempt_id(&self) -> u64 {
        match self {
            Self::Gathered { attempt_id }
            | Self::ReflexiveCandidate { attempt_id }
            | Self::LaneOpen { attempt_id, .. }
            | Self::LaneFailed { attempt_id, .. }
            | Self::Bytes { attempt_id, .. }
            | Self::IceFailed { attempt_id, .. }
            | Self::Measured { attempt_id, .. } => *attempt_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::{PEER_EVENT_BACKLOG, PeerEvent, PeerEventSink};
    use roost_client_core::client::carriers::PeerLane;

    #[test]
    fn the_host_is_rung_after_the_event_it_must_drain_is_queued() {
        let sink = PeerEventSink::new();
        let handler_copy = sink.clone();
        let drained = Rc::new(RefCell::new(Vec::new()));
        let overflowed = Rc::new(RefCell::new(Vec::new()));
        let notify: Rc<dyn Fn()> = {
            let sink = sink.clone();
            let drained = Rc::clone(&drained);
            let overflowed = Rc::clone(&overflowed);
            Rc::new(move || {
                drained.borrow_mut().extend(sink.drain());
                overflowed
                    .borrow_mut()
                    .extend(sink.take_overflowed().attempt_ids);
            })
        };
        sink.notify_on_record(notify);

        handler_copy.record(PeerEvent::Bytes {
            attempt_id: 4,
            lane: PeerLane::Data,
            bytes: vec![7],
        });
        assert_eq!(
            *drained.borrow(),
            vec![PeerEvent::Bytes {
                attempt_id: 4,
                lane: PeerLane::Data,
                bytes: vec![7],
            }],
            "a clone made before the notify was set rings it, and the ring finds its own event"
        );

        for _ in 0..PEER_EVENT_BACKLOG {
            sink.enqueue(PeerEvent::Gathered { attempt_id: 5 });
        }
        handler_copy.record(PeerEvent::IceFailed {
            attempt_id: 6,
            reason: "gone".to_owned(),
        });
        assert_eq!(
            *overflowed.borrow(),
            vec![6],
            "an event the full queue dropped still rings, so its attempt is retired at once"
        );
    }

    #[test]
    fn events_are_drained_in_arrival_order_and_then_the_queue_is_empty() {
        let sink = PeerEventSink::new();
        sink.record(PeerEvent::Gathered { attempt_id: 1 });
        sink.record(PeerEvent::LaneOpen {
            attempt_id: 1,
            lane: PeerLane::Control,
        });
        sink.record(PeerEvent::Bytes {
            attempt_id: 1,
            lane: PeerLane::Control,
            bytes: vec![1, 2, 3],
        });

        assert_eq!(
            sink.drain(),
            vec![
                PeerEvent::Gathered { attempt_id: 1 },
                PeerEvent::LaneOpen {
                    attempt_id: 1,
                    lane: PeerLane::Control,
                },
                PeerEvent::Bytes {
                    attempt_id: 1,
                    lane: PeerLane::Control,
                    bytes: vec![1, 2, 3],
                },
            ]
        );
        assert!(
            sink.drain().is_empty(),
            "a drained queue is empty, so a host that drains twice does not settle twice"
        );
    }

    #[test]
    fn a_full_queue_drops_the_event_and_names_the_attempt_to_retire() {
        let sink = PeerEventSink::new();
        for _ in 0..PEER_EVENT_BACKLOG {
            sink.record(PeerEvent::Gathered { attempt_id: 1 });
        }
        sink.record(PeerEvent::Bytes {
            attempt_id: 7,
            lane: PeerLane::Data,
            bytes: vec![9],
        });

        assert_eq!(
            sink.drain().len(),
            PEER_EVENT_BACKLOG,
            "the bound is what the queue holds before it refuses"
        );
        let overflow = sink.take_overflowed();
        assert_eq!(overflow.attempt_ids, vec![7]);
        assert!(
            sink.take_overflowed().attempt_ids.is_empty(),
            "the overflow is reported once: a drain must not re-retire a peer"
        );
    }

    #[test]
    fn one_attempt_overflowing_is_reported_once_however_many_events_it_lost() {
        let sink = PeerEventSink::new();
        for _ in 0..PEER_EVENT_BACKLOG {
            sink.record(PeerEvent::Gathered { attempt_id: 3 });
        }
        for _ in 0..5 {
            sink.record(PeerEvent::IceFailed {
                attempt_id: 3,
                reason: "ice failed".to_owned(),
            });
        }

        assert_eq!(
            sink.take_overflowed().attempt_ids,
            vec![3],
            "the retire is per attempt, not per lost event"
        );
    }
}
