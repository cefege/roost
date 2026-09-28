//! F5: the six frames the daemon already served and the client could not ask
//! for, against a REAL keeper daemon.
//!
//! Every expectation below is transcribed from the daemon handler that
//! produces the answer, cited by line, and never from the client method under
//! test. A fixture that asks the client what the keeper would say is `x == x`.
//! Two rules follow from the audit that found these, and both are load-bearing
//! here:
//!
//! * A test whose expected value equals the REQUESTED value is not a test of a
//!   query — it is a test that nothing happened. The discriminating case for
//!   geometry is a STALE sequence: `pty_channel.rs:200` returns the applied
//!   sequence unchanged, so a lower-sequence request is acknowledged with the
//!   FIRST geometry, and a client that echoes its own request cannot produce
//!   that answer.
//! * A frame's meaning comes from the byte on the wire, not from the nearest
//!   vocabulary in the crate. `GetHistory` is answered with `GetHistoryResp`
//!   `[head:u64][ring]` and `GetHistoryRecords` with the versioned ordered
//!   records (v2 `keeper-frame-handler.ts:487-520`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::time::{Duration, Instant};

use roost_keeper::client::KeeperClient;
use roost_keeper::client::connect;
use roost_keeper::history::{HistoryRecord, HistoryRecords};
use roost_keeper::payloads::TerminalState;
use support::daemon::{Keeper, TempDir, wait_until};
use support::{echo, idle};

/// A connected client holding one live channel, which every query needs.
fn client_with_channel(temp: &TempDir) -> (Keeper, KeeperClient) {
    let keeper = Keeper::start(temp);
    let client = connect(keeper.socket()).expect("a handshake");
    let pid = client.spawn(1, echo(), 80, 24).expect("a spawn");
    assert!(pid > 0, "the channel is a real child");
    (keeper, client)
}

/// F5 — the ordered history an adopter replays: the head, the base geometry the
/// channel was spawned at, and the marker the keeper recorded at the sequence
/// it applied (v2 `appendResizeHistory`).
#[test]
fn the_ordered_history_is_the_resize_the_keeper_recorded() {
    let temp = TempDir::new("history");
    let (_keeper, client) = client_with_channel(&temp);

    assert_eq!(
        client.resize(1, 9, 132, 43).applied_geometry(),
        Some((132, 43)),
        "the resize the history will record"
    );
    let history = client.history_records(1).expect("the retained history").history;

    assert_eq!(
        history,
        HistoryRecords {
            head_seq: 0,
            base_cols: 80,
            base_rows: 24,
            records: vec![HistoryRecord::Resize { seq: 9, cols: 132, rows: 43 }],
        },
        "nothing was emitted, the base is the spawn geometry, and the marker is at the applied sequence"
    );
}

/// F5 — the legacy question is answered with `GetHistoryResp`: the head and the
/// raw ring, which agree with the ordered answer's head and window.
#[test]
fn the_legacy_history_is_the_head_and_the_raw_ring() {
    let temp = TempDir::new("legacy-history");
    let (_keeper, client) = client_with_channel(&temp);

    client.write_input(1, b"legacy-ring\n").expect("input written");
    let mut legacy = (0, Vec::new());
    wait_until("the echo is retained", || {
        legacy = client.legacy_history(1).expect("the legacy answer arrives");
        String::from_utf8_lossy(&legacy.1).contains("legacy-ring")
    });
    let ordered = client.history_records(1).expect("the ordered answer arrives").history;

    assert!(legacy.0 > 0 && legacy.0 >= legacy.1.len() as u64, "the head counts every emitted byte");
    assert!(ordered.head_seq >= legacy.0, "the head never goes backwards");
    assert!(
        String::from_utf8_lossy(&ordered.window()).contains("legacy-ring"),
        "the ordered window holds the same retained bytes"
    );
}

/// The history answer is an ORDERED boundary: every `PtyOut` the channel sent
/// before the answer is already inside it and is dropped by the wait, so the
/// dropped bytes across successive reads add up to exactly the head (v2
/// `releasePendingHistoryOutput`). A client that deferred them instead would
/// replay those bytes twice.
#[test]
fn pre_boundary_output_is_dropped_exactly_once() {
    let temp = TempDir::new("history-boundary");
    let (_keeper, client) = client_with_channel(&temp);

    client.write_input(1, b"boundary-marker\n").expect("input written");
    let mut dropped = 0usize;
    let mut last = HistoryRecords::unknown_channel();
    wait_until("the echo is retained", || {
        let bounded = client.history_records(1).expect("the ordered answer arrives");
        dropped += bounded.dropped_output_bytes;
        last = bounded.history;
        String::from_utf8_lossy(&last.window()).contains("boundary-marker")
    });

    assert_eq!(dropped as u64, last.head_seq, "every emitted byte crossed the boundary once");
}

/// F5 — the authoritative geometry, against a STALE sequence.
///
/// `apply_resize` (`pty_channel.rs:200`) refuses to move backwards and returns
/// the sequence already applied, and the daemon acknowledges with the PTY's own
/// state (`keeper_ops.rs:227-233`). So after a resize to 132x43 at sequence 9,
/// a request for 200x60 at sequence 4 is acknowledged with 132x43. A client that
/// answered from its own request would say 200x60 — which is the specific lie
/// this whole test exists to catch.
#[test]
fn the_terminal_state_is_the_geometry_the_keeper_applied_not_the_one_asked_for() {
    let temp = TempDir::new("state");
    let (_keeper, client) = client_with_channel(&temp);

    assert_eq!(client.resize(1, 9, 132, 43).seq(), 9);
    // Stale: refused as a change, acknowledged as a no-op at what is applied.
    let stale = client.resize(1, 4, 200, 60);
    assert_eq!(
        stale.applied_geometry(),
        Some((132, 43)),
        "a stale sequence keeps the geometry the keeper already had"
    );

    let state: TerminalState = client.terminal_state(1).expect("the live state");
    assert_eq!(
        (state.applied_seq, state.cols, state.rows),
        (9, 132, 43),
        "the keeper answers GetTerminalState from live channel state \
         (keeper_ops.rs:260), and 200x60 is what nobody ever applied"
    );
}

/// F5 — `ResizeStatus` is the RECOVERY for a lost acknowledgement.
///
/// It is answered from the same live state (`keeper_ops.rs:246`) and it never
/// re-applies geometry, so asking for a sequence that was never sent is
/// harmless and must not move the PTY. Asserted both ways: the answer, and the
/// absence of any effect.
#[test]
fn the_resize_status_recovers_the_applied_geometry_without_applying_anything() {
    let temp = TempDir::new("status");
    let (_keeper, client) = client_with_channel(&temp);

    client.resize(1, 5, 100, 30);
    let recovered = client.resize_status(1, 5).expect("the cached status");
    assert_eq!(
        (recovered.applied_seq, recovered.cols, recovered.rows),
        (5, 100, 30),
        "a worker that lost the acknowledgement learns what landed"
    );

    // A sequence the keeper was never asked about. The daemon answers from
    // live state, so this reports the truth rather than inventing an answer.
    let never_sent = client
        .resize_status(1, 999)
        .expect("the status of a sequence that was never sent");
    assert_eq!(
        (never_sent.applied_seq, never_sent.cols, never_sent.rows),
        (5, 100, 30),
        "asking is not applying: the PTY is still where the last resize left it"
    );
    let after = client.terminal_state(1).expect("the live state");
    assert_eq!(after, recovered, "and the keeper's own state did not move");
}

/// F5 — terminating a channel's child.
///
/// `KillChild` is served with NO reply (`keeper.rs:230` returns an empty
/// `Vec`), so a client that waited for one would sit out its whole timeout on
/// every close. What proves the kill landed is the channel leaving the
/// daemon's own channel list, which is the keeper reaping the exited child
/// (`keeper.rs:275`).
#[test]
fn killing_a_channel_ends_its_child_and_waits_for_no_reply() {
    let temp = TempDir::new("kill");
    let keeper = Keeper::start(&temp);
    let client = connect(keeper.socket()).expect("a handshake");
    client.spawn(1, idle(), 80, 24).expect("a spawn");

    let start = Instant::now();
    client.kill(1).expect("the kill frame is written");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "kill waited for a reply the daemon never sends (keeper.rs:230)"
    );

    wait_until("the keeper stops listing the killed channel", || {
        client
            .list_channels()
            .map(|list| list.channels.iter().all(|binding| binding.channel_id != 1))
            .unwrap_or(false)
    });
}

/// F5 — the conditional shutdown, both answers.
///
/// The daemon checks its channel count and answers in one operation
/// (`keeper_ops.rs:70`): `ShutdownIfEmptyAck` when empty, and
/// `ShutdownIfEmptyReject` when a channel is live. The pair is the whole test —
/// a client that always answered `false` would pass the first and fail the
/// second. The refusal is also asserted to arrive FAST, because a client that
/// waited only for the ack could not see the reject and would pay the full
/// timeout to learn the same thing: a timeout is a wedged keeper, and it is not
/// a rejection.
#[test]
fn a_conditional_shutdown_is_refused_while_the_keeper_holds_a_channel() {
    let temp = TempDir::new("busy");
    let (mut keeper, client) = client_with_channel(&temp);

    let start = Instant::now();
    let accepted = client.shutdown_if_empty().expect("the keeper answers");
    assert!(
        !accepted,
        "a keeper holding a live channel must not retire (keeper_ops.rs:70)"
    );
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "the refusal is a frame the daemon sends, not a silence to be timed out; \
         it took {:?}",
        start.elapsed()
    );
    assert!(
        !keeper.has_exited(),
        "a refused conditional shutdown leaves the keeper running"
    );
    // The daemon ends the CONNECTION for this exchange whatever it answers
    // (`server.rs:322`), so the surviving PTY is read from a fresh one. That
    // the keeper is still there at all is the point: a refusal that took the
    // process with it would have retired the terminals it just declined to
    // retire.
    let reconnected = connect(keeper.socket()).expect("the keeper is still serving");
    assert_eq!(
        reconnected
            .list_channels()
            .expect("a channel list")
            .channels
            .len(),
        1,
        "and the live PTY is untouched"
    );
}

/// F5 — the same question on an empty keeper, which the daemon retires.
#[test]
fn an_empty_keeper_retires_on_a_conditional_shutdown() {
    let temp = TempDir::new("empty");
    let mut keeper = Keeper::start(&temp);
    let client = connect(keeper.socket()).expect("a handshake");

    assert!(
        client.shutdown_if_empty().expect("the keeper answers"),
        "an empty keeper accepts the conditional shutdown (keeper_ops.rs:70)"
    );
    wait_until("the daemon exits", || keeper.has_exited());
}

/// F5 — the unconditional shutdown, which is the destructive one.
///
/// The daemon stops and every PTY it owned dies with it
/// (`bin/roost-keeper.rs:125`), so this is only ever an operator's deliberate
/// offline maintenance. The assertion is that the daemon acknowledged AND
/// stopped, because an acknowledgement alone would leave a caller believing a
/// keeper had retired while it is still holding every terminal on the machine.
#[test]
fn an_unconditional_shutdown_stops_the_keeper() {
    let temp = TempDir::new("shutdown");
    let mut keeper = Keeper::start(&temp);
    let client = connect(keeper.socket()).expect("a handshake");

    client.shutdown().expect("the keeper acknowledges");
    wait_until("the daemon exits", || keeper.has_exited());
}
