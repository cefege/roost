//! The three `WorkerRouteIndex` methods: lookup, the exact replacement a
//! worker's snapshot performs, and what a retirement names.
//!
//! These are the properties the workers domain depends on and cannot see from
//! its own side: a hello's exact snapshot, a delete's route sweep, and a frame
//! dispatch's lookup. The atomicity assertion is the one that would be
//! impossible to write against a per-key implementation, which is the whole
//! reason `byte-hub.ts` was split on this seam. What arrives on a route is
//! `terminal_screen_byte_hub.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use roost_coord::coord_core::seams::{LiveChannel, WorkerRouteIndex};
use roost_coord::terminal_screen::byte_hub::ByteHub;
use roost_coord::terminal_screen::replica::ScreenHub;
use roost_coord::terminal_screen::route_index::{RouteRetirement, RouteRetirementSink};
use roost_protocol::wire::{ChannelId, SessionId, WorkerFp};

const WORKER_A: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
const WORKER_B: &str = "bb00000000000000000000000000000000000000000000000000000000000000";

fn worker(raw: &str) -> WorkerFp {
    WorkerFp::try_from(raw).unwrap()
}

fn session(raw: &str) -> SessionId {
    SessionId::try_from(raw).unwrap()
}

fn uuid(tail: &str) -> String {
    format!("00000000-0000-4000-8000-{tail:0>12}")
}

fn channel(raw: i64) -> ChannelId {
    ChannelId::try_from(raw).unwrap()
}

/// Records every retirement, so a test can name what stopped resolving.
#[derive(Default)]
struct RecordingSink(Mutex<Vec<RouteRetirement>>);

impl RouteRetirementSink for RecordingSink {
    fn route_retired(&self, retirement: &RouteRetirement) {
        self.0.lock().unwrap().push(retirement.clone());
    }
}

impl RecordingSink {
    fn keys(&self) -> Vec<(String, u32, String)> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    entry.worker_fp.as_str().to_owned(),
                    entry.channel_id.as_u32(),
                    entry.session_id.as_str().to_owned(),
                )
            })
            .collect()
    }
}

fn hub(sink: &Arc<RecordingSink>) -> ByteHub {
    ByteHub::new(
        Arc::new(ScreenHub::new()),
        Arc::clone(sink) as Arc<dyn RouteRetirementSink>,
    )
}

#[test]
fn lookup_session_id_answers_the_route_a_worker_channel_carries() {
    let sink = Arc::new(RecordingSink::default());
    let hub = hub(&sink);
    let (live, gone) = (session(&uuid("1")), session(&uuid("2")));

    hub.replace_worker_channel_index(
        &worker(WORKER_A),
        &[LiveChannel {
            session_id: live.clone(),
            channel_id: channel(7),
        }],
    );

    assert_eq!(
        hub.lookup_session_id(&worker(WORKER_A), &channel(7)),
        Some(live.clone())
    );
    // An empty index is a real answer meaning "no live route", not a failure.
    assert_eq!(hub.lookup_session_id(&worker(WORKER_A), &channel(8)), None);
    assert_eq!(hub.lookup_session_id(&worker(WORKER_B), &channel(7)), None);
    assert_eq!(gone, session(&uuid("2")));
    assert!(sink.keys().is_empty(), "installing a route retires nothing");
}

#[test]
fn replace_worker_channel_index_drops_every_route_not_in_the_new_list() {
    let sink = Arc::new(RecordingSink::default());
    let hub = hub(&sink);
    let (kept, dropped_a, dropped_b) = (
        session(&uuid("1")),
        session(&uuid("2")),
        session(&uuid("3")),
    );

    hub.replace_worker_channel_index(
        &worker(WORKER_A),
        &[
            LiveChannel {
                session_id: dropped_a.clone(),
                channel_id: channel(1),
            },
            LiveChannel {
                session_id: dropped_b.clone(),
                channel_id: channel(2),
            },
            LiveChannel {
                session_id: kept.clone(),
                channel_id: channel(3),
            },
        ],
    );
    hub.replace_worker_channel_index(
        &worker(WORKER_A),
        &[LiveChannel {
            session_id: kept.clone(),
            channel_id: channel(3),
        }],
    );

    assert_eq!(
        hub.lookup_session_id(&worker(WORKER_A), &channel(3)),
        Some(kept)
    );
    assert_eq!(
        hub.lookup_session_id(&worker(WORKER_A), &channel(1)),
        None,
        "WORKER_A:1 was dropped by the replacement"
    );
    assert_eq!(
        hub.lookup_session_id(&worker(WORKER_A), &channel(2)),
        None,
        "WORKER_A:2 was dropped by the replacement"
    );
    assert_eq!(
        sink.keys(),
        vec![
            (WORKER_A.to_owned(), 1, dropped_a.as_str().to_owned()),
            (WORKER_A.to_owned(), 2, dropped_b.as_str().to_owned()),
        ],
        "each dropped route is retired exactly once, naming the session it used to carry"
    );
}

#[test]
fn a_replacement_leaves_another_workers_routes_untouched() {
    let sink = Arc::new(RecordingSink::default());
    let hub = hub(&sink);
    let (a_session, b_session) = (session(&uuid("1")), session(&uuid("2")));

    hub.replace_worker_channel_index(
        &worker(WORKER_A),
        &[LiveChannel {
            session_id: a_session.clone(),
            channel_id: channel(1),
        }],
    );
    hub.replace_worker_channel_index(
        &worker(WORKER_B),
        &[LiveChannel {
            session_id: b_session.clone(),
            channel_id: channel(1),
        }],
    );
    hub.replace_worker_channel_index(&worker(WORKER_A), &[]);

    assert_eq!(hub.lookup_session_id(&worker(WORKER_A), &channel(1)), None);
    assert_eq!(
        hub.lookup_session_id(&worker(WORKER_B), &channel(1)),
        Some(b_session),
        "one worker's snapshot never reconciles another's routes"
    );
    assert_eq!(sink.keys().len(), 1, "only WORKER_A:1 was retired");
}

#[test]
fn a_rebinding_session_loses_its_older_key_on_the_same_worker() {
    let sink = Arc::new(RecordingSink::default());
    let hub = hub(&sink);
    let moved = session(&uuid("1"));

    hub.replace_worker_channel_index(
        &worker(WORKER_A),
        &[LiveChannel {
            session_id: moved.clone(),
            channel_id: channel(4),
        }],
    );
    hub.replace_worker_channel_index(
        &worker(WORKER_A),
        &[LiveChannel {
            session_id: moved.clone(),
            channel_id: channel(9),
        }],
    );

    assert_eq!(
        hub.lookup_session_id(&worker(WORKER_A), &channel(9)),
        Some(moved.clone())
    );
    assert_eq!(
        hub.lookup_session_id(&worker(WORKER_A), &channel(4)),
        None,
        "a rebound session's old channel must stop resolving, or its cells land on the wrong replica"
    );
    assert_eq!(
        sink.keys(),
        vec![(WORKER_A.to_owned(), 4, moved.as_str().to_owned())]
    );
}

#[test]
fn a_replacement_is_atomic_for_a_concurrent_reader() {
    let sink = Arc::new(RecordingSink::default());
    let hub = hub(&sink);
    // Two states, deliberately disjoint, so a half-applied replacement is
    // observable as a third answer rather than as a plausible one.
    let (old_session, new_session) = (session(&uuid("1")), session(&uuid("2")));
    let old_index = [LiveChannel {
        session_id: old_session.clone(),
        channel_id: channel(1),
    }];
    let new_index = [LiveChannel {
        session_id: new_session.clone(),
        channel_id: channel(2),
    }];
    hub.replace_worker_channel_index(&worker(WORKER_A), &old_index);

    let old_whole = BTreeMap::from([(channel(1), old_session.clone())]);
    let new_whole = BTreeMap::from([(channel(2), new_session.clone())]);
    let observed = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicUsize::new(0));
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let hub = hub.clone();
            let observed = Arc::clone(&observed);
            let stop = Arc::clone(&stop);
            let old_whole = old_whole.clone();
            let new_whole = new_whole.clone();
            std::thread::spawn(move || {
                while stop.load(Ordering::Relaxed) == 0 {
                    let whole = hub.worker_routes(&worker(WORKER_A));
                    // One whole index per read, so a half-applied replacement
                    // would show as a third shape rather than as a plausible one.
                    assert!(
                        whole == old_whole || whole == new_whole,
                        "a reader saw a half-replaced index: {whole:?}"
                    );
                    observed.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();
    for _ in 0..2_000 {
        hub.replace_worker_channel_index(&worker(WORKER_A), &new_index);
        hub.replace_worker_channel_index(&worker(WORKER_A), &old_index);
    }
    stop.store(1, Ordering::Relaxed);
    for reader in readers {
        reader.join().unwrap();
    }
    assert!(
        observed.load(Ordering::Relaxed) > 0,
        "the readers never ran, so the assertion above proved nothing"
    );
}

#[test]
fn retire_worker_routes_names_the_sessions_that_lost_a_route() {
    let sink = Arc::new(RecordingSink::default());
    let hub = hub(&sink);
    let (one, two, other_worker) = (
        session(&uuid("1")),
        session(&uuid("2")),
        session(&uuid("3")),
    );

    hub.replace_worker_channel_index(
        &worker(WORKER_A),
        &[
            LiveChannel {
                session_id: one.clone(),
                channel_id: channel(1),
            },
            LiveChannel {
                session_id: two.clone(),
                channel_id: channel(2),
            },
        ],
    );
    hub.replace_worker_channel_index(
        &worker(WORKER_B),
        &[LiveChannel {
            session_id: other_worker.clone(),
            channel_id: channel(1),
        }],
    );

    let mut affected = hub.retire_worker_routes(&worker(WORKER_A));
    affected.sort();

    let mut expected = vec![one.clone(), two.clone()];
    expected.sort();
    assert_eq!(
        affected, expected,
        "both of WORKER_A's sessions lost a route, and are named once each"
    );
    assert_eq!(hub.lookup_session_id(&worker(WORKER_A), &channel(1)), None);
    assert_eq!(hub.lookup_session_id(&worker(WORKER_A), &channel(2)), None);
    assert_eq!(
        hub.lookup_session_id(&worker(WORKER_B), &channel(1)),
        Some(other_worker),
        "retiring one worker leaves the rest of the hub alone"
    );
    assert_eq!(
        hub.cached_route(&two),
        None,
        "the route cache entry of a retired session is gone with its route"
    );
}

#[test]
fn a_retirement_twice_names_nothing_the_second_time() {
    let sink = Arc::new(RecordingSink::default());
    let hub = hub(&sink);
    let only = session(&uuid("1"));
    hub.replace_worker_channel_index(
        &worker(WORKER_A),
        &[LiveChannel {
            session_id: only,
            channel_id: channel(1),
        }],
    );

    assert_eq!(hub.retire_worker_routes(&worker(WORKER_A)).len(), 1);
    assert!(
        hub.retire_worker_routes(&worker(WORKER_A)).is_empty(),
        "an already-retired worker has no routes left to name, so the caller has no cleanup to repeat"
    );
}

#[test]
fn a_reconciled_worker_is_marked_and_a_reconnect_reopens_the_window() {
    let sink = Arc::new(RecordingSink::default());
    let hub = hub(&sink);

    assert!(!hub.is_worker_channel_index_reconciled(&worker(WORKER_A)));
    hub.replace_worker_channel_index(&worker(WORKER_A), &[]);
    assert!(hub.is_worker_channel_index_reconciled(&worker(WORKER_A)));
    hub.reset_worker_channel_index_reconcile(&worker(WORKER_A));
    assert!(
        !hub.is_worker_channel_index_reconciled(&worker(WORKER_A)),
        "a fresh hello re-primes the index, so the worker is not reconciled until its snapshot lands"
    );
}
