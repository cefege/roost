//! The per-session unhandled-sequence log against a real core: a sequence the
//! core dropped is reported once per core, distinct parameters are distinct
//! sequences, the log stops at its cap and says so, and entries the core's
//! ring overwrote before a sample are counted rather than invented. Ports the
//! behaviour of `apps/worker/src/session/session-unhandled-seq.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_host::HostPlatform;
use roost_term::{AlacrittyCore, CellEmitState};
use roost_worker::event_store::{DurableEventKind, Store};
use roost_worker::session::history::UNHANDLED_SEQ_MAX;
use roost_worker::session::ring::ScrollbackRing;
use roost_worker::session::types::{SessionIdentity, SessionRecord};
use roost_worker::session::unhandled_seq::{
    UnhandledSequenceSnapshotEntry, note_unhandled_sequences, unhandled_sequence_snapshot,
};
use roost_worker::shell_spec::ShellSpec;

fn record() -> SessionRecord {
    let mut store = Store::new();
    let reservation = store
        .reserve(DurableEventKind::Closed, 64)
        .expect("the store admits a close claim");
    SessionRecord::new(
        SessionIdentity {
            session_id: "00000000-0000-4000-8000-0000000000aa"
                .try_into()
                .expect("a uuid is a session id"),
            channel_id: 3i64.try_into().expect("a positive id is a channel id"),
            socket_path: "mux:3".to_string(),
            cwd: "/".to_string(),
            shell_spec: ShellSpec {
                version: 1,
                platform: HostPlatform::Linux,
                executable: "/bin/sh".to_string(),
                argv: Vec::new(),
                cwd: "/".to_string(),
                env: Vec::new(),
            },
            session_trace_id: "0000beef0000beef".try_into().expect("a trace id"),
            spawned_at_ms: 0,
        },
        reservation,
        Box::new(AlacrittyCore::new(80, 24)),
        CellEmitState::new("epoch", "stream"),
        ScrollbackRing::default(),
    )
}

/// `CSI > n W` has no handler, so each `n` is a distinct unhandled sequence.
fn distinct_unhandled(count: u16) -> Vec<u8> {
    (0..count)
        .flat_map(|index| format!("\x1b[>{index}W").into_bytes())
        .collect()
}

#[test]
fn a_core_that_dropped_nothing_leaves_no_log() {
    let mut session = record();
    session
        .terminal_core
        .write(b"\x1b[1;31mhello\x1b[0m\x1b[2J\x1b[6n");
    assert_eq!(unhandled_sequence_snapshot(&mut session, 5), None);
    assert!(
        session.unhandled.is_none(),
        "a healthy terminal allocates nothing"
    );
}

#[test]
fn a_dropped_sequence_is_reported_once_per_core_with_what_the_core_recorded() {
    let mut session = record();
    session.terminal_core.write(b"\x1b[7 q\x1b[7 q");
    note_unhandled_sequences(&mut session, 10);
    session.terminal_core.write(b"\x1b[7 q");
    let snapshot = unhandled_sequence_snapshot(&mut session, 20).expect("the core logged");
    assert_eq!(
        snapshot.entries,
        vec![UnhandledSequenceSnapshotEntry {
            final_byte: "q".to_string(),
            private: String::new(),
            param_count: 1,
            params: vec![7],
            first_seen_mono_ms: 10,
        }]
    );
    assert_eq!(
        snapshot.logged_total, 3,
        "repeats count toward the total only"
    );
    assert_eq!(snapshot.ring_dropped, 0);
    assert!(!snapshot.capped);
}

#[test]
fn one_final_byte_with_different_parameters_is_two_sequences() {
    let mut session = record();
    session
        .terminal_core
        .write(b"\x1b[>1;2;3;4;5W\x1b[>1;2;3;4W");
    let snapshot = unhandled_sequence_snapshot(&mut session, 1).expect("the core logged");
    let shapes: Vec<_> = snapshot
        .entries
        .iter()
        .map(|entry| {
            (
                entry.private.as_str(),
                entry.param_count,
                entry.params.clone(),
            )
        })
        .collect();
    assert_eq!(
        shapes,
        vec![(">", 5, vec![1, 2, 3, 4]), (">", 4, vec![1, 2, 3, 4])],
        "only four parameters are recorded, so the count is what tells them apart"
    );
}

/// XTSMGRAPHICS never reaches the core (vte dispatches `CSI S` only without a
/// private marker), but the worker answers it, so it is not a gap to report.
#[test]
fn a_probe_the_worker_answers_is_not_reported_as_unhandled() {
    let mut session = record();
    session.terminal_core.write(b"\x1b[?2;1;0S\x1b[?7;9;9Z");
    let snapshot = unhandled_sequence_snapshot(&mut session, 1).expect("the core logged");
    let shapes: Vec<_> = snapshot
        .entries
        .iter()
        .map(|entry| (entry.private.as_str(), entry.params.clone()))
        .collect();
    assert_eq!(shapes, vec![("?", vec![7, 9, 9])]);
}

#[test]
fn the_log_stops_at_its_cap_and_says_it_did() {
    let mut session = record();
    for batch in 0..4u16 {
        let bytes: Vec<u8> = (batch * 10..batch * 10 + 10)
            .flat_map(|index| format!("\x1b[>{index}W").into_bytes())
            .collect();
        session.terminal_core.write(&bytes);
        note_unhandled_sequences(&mut session, u64::from(batch));
    }
    let snapshot = unhandled_sequence_snapshot(&mut session, 9).expect("the core logged");
    assert_eq!(snapshot.entries.len(), UNHANDLED_SEQ_MAX);
    assert!(snapshot.capped, "the cap is reported, not hidden");
    assert_eq!(
        snapshot.logged_total, 40,
        "the mark keeps advancing past the cap"
    );
    assert_eq!(
        snapshot.ring_dropped, 0,
        "every sample read its window in time"
    );
}

#[test]
fn entries_the_core_ring_overwrote_before_a_sample_are_counted() {
    let mut session = record();
    session.terminal_core.write(&distinct_unhandled(40));
    let snapshot = unhandled_sequence_snapshot(&mut session, 1).expect("the core logged");
    assert_eq!(
        snapshot.ring_dropped, 8,
        "40 logged against a 32-entry window"
    );
    assert_eq!(
        snapshot.entries.first().map(|entry| entry.params.clone()),
        Some(vec![8])
    );
    assert_eq!(snapshot.logged_total, 40);
}
