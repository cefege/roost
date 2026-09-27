//! The terminal half of a committed event, and the two things it must actually
//! do.
//!
//! **THE FIRST TEST IS THE MUTATION TEST FOR A WHOLE CLASS.** `Opened` and
//! `Respawned` are DELTAS and `replace_worker_channel_index` is a REPLACEMENT,
//! so an implementation that reaches for the replacement on a delta silently
//! erases every other live channel on that worker — and that erasure is
//! invisible until a cell frame arrives for a channel the route cache has
//! forgotten. Two `Opened`s on one worker, with the first asserted routable
//! after the second, is the only assertion that catches it.
//!
//! `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex, PoisonError};

use roost_coord::coord_core::seams::WorkerRouteIndex;
use roost_coord::events::append::LiveEffects;
use roost_coord::terminal_screen::byte_hub::ByteHub;
use roost_coord::terminal_screen::live_effects::{OrphanPtyKill, TerminalLiveEffects};
use roost_protocol::wire::{ChannelId, SessionEvent, SessionId, WorkerFp};

/// Every kill that was asked for, so the test can assert one reached a PTY.
#[derive(Debug, Default)]
struct RecordingKills(Mutex<Vec<(String, String)>>);

impl OrphanPtyKill for RecordingKills {
    fn kill(&self, worker_fp: &WorkerFp, session_id: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((worker_fp.as_str().to_owned(), session_id.to_owned()));
    }
}

/// A well-formed session id, since the brand refuses anything else.
fn session_id(n: u8) -> SessionId {
    SessionId::try_from(format!("00000000-0000-4000-8000-0000000000{n:02}"))
        .expect("a well-formed session id")
}

fn channel(n: i64) -> ChannelId {
    ChannelId::try_from(n).expect("a real channel id")
}

fn worker() -> WorkerFp {
    WorkerFp::try_from("a".repeat(64)).expect("a 64-hex fingerprint")
}

fn opened(session: u8, on_channel: i64) -> SessionEvent {
    SessionEvent::Opened {
        session_id: session_id(session),
        worker_fp: worker(),
        channel: channel(on_channel),
        session_kind: roost_protocol::wire::SessionKind::Shell,
        cwd: "/tmp".to_owned(),
        ts: 0,
        trace_id: None,
    }
}

fn effects() -> (TerminalLiveEffects, Arc<ByteHub>, Arc<RecordingKills>) {
    let hub = Arc::new(ByteHub::with_defaults());
    let kills = Arc::new(RecordingKills::default());
    let effects = TerminalLiveEffects::new(
        Arc::clone(&hub),
        Arc::clone(&kills) as Arc<dyn OrphanPtyKill>,
    );
    (effects, hub, kills)
}

#[test]
fn a_second_opened_does_not_erase_the_first_workers_channel() {
    let (effects, hub, _kills) = effects();
    let worker = worker();

    effects.index_durable_channel(&opened(1, 10), None);
    effects.index_durable_channel(&opened(2, 20), None);

    // The mutation: a `replace_worker_channel_index` implementation of either
    // delta arm makes the FIRST lookup miss, and the failure surfaces much later
    // as a cell frame for channel 10 that nobody can place.
    assert_eq!(
        hub.lookup_session_id(&worker, &channel(10)).as_ref(),
        Some(&session_id(1)),
        "the first channel must still route after a second one is bound"
    );
    assert_eq!(
        hub.lookup_session_id(&worker, &channel(20)).as_ref(),
        Some(&session_id(2))
    );
}

#[test]
fn a_respawn_rebinds_only_when_the_caller_is_an_authenticated_worker() {
    let (effects, hub, _kills) = effects();
    let worker = worker();
    let respawn = |on_channel: i64| SessionEvent::Respawned {
        session_id: session_id(1),
        new_channel: channel(on_channel),
        ts: 0,
        trace_id: None,
    };

    // `None` is a coordinator-side producer, and the trait's rule is that it
    // binds NOTHING: inferring the worker from the route cache could bind on a
    // worker that has already been replaced.
    effects.index_durable_channel(&respawn(30), None);
    assert!(
        hub.lookup_session_id(&worker, &channel(30)).is_none(),
        "an unauthenticated producer must not bind a respawn to any worker"
    );

    effects.index_durable_channel(&respawn(30), Some(&worker));
    assert_eq!(
        hub.lookup_session_id(&worker, &channel(30)).as_ref(),
        Some(&session_id(1))
    );
}

#[test]
fn a_kill_reaches_a_pty() {
    let (effects, _hub, kills) = effects();
    effects.kill_orphan_pty(&worker(), "00000000-0000-4000-8000-000000000007");
    assert_eq!(
        kills
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_slice(),
        &[(
            "a".repeat(64),
            "00000000-0000-4000-8000-000000000007".to_owned(),
        )],
        "a reap that reaches nothing leaves a session row that outlives its \
         process, and nothing else in the tree reports it"
    );
}

#[test]
fn an_event_that_names_no_channel_binds_nothing() {
    let (effects, hub, _kills) = effects();
    let worker = worker();
    effects.index_durable_channel(&opened(1, 10), None);
    let channel = channel(10);

    effects.index_durable_channel(
        &SessionEvent::Renamed {
            session_id: session_id(1),
            custom_title: "renamed".to_owned(),
            ts: 0,
            trace_id: None,
        },
        None,
    );
    assert_eq!(
        hub.lookup_session_id(&worker, &channel).as_ref(),
        Some(&session_id(1)),
        "a variant with no ChannelId must not disturb the index"
    );
}
