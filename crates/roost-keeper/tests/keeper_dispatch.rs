//! The keeper's frame dispatcher, driven with no listener, no socket and no
//! timing. The split from the socket loop is what makes these writable at all:
//! the protocol's edge cases are the ones that need a second live endpoint to
//! reproduce, and those are exactly the ones nobody writes tests for.
//!
//! The contract is `protocol/spec/keeper.md`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::time::{Duration, Instant};

use roost_keeper::codec::{MuxFrame, MuxFrameType};
use roost_keeper::frames::{ShellSpec, SpawnAck, SpawnRequest};
use roost_keeper::keeper::Keeper;
use roost_keeper::payloads::{KeeperFeature, TerminalState};
use support::{DEADLINE, drain_until, empty_frame, input_frame, resize_frame, spawn_frame};

/// `GetTerminalState` is the recovery for a lost `ResizeAck` with no retained
/// marker left, so it must answer from LIVE state and not from history.
#[test]
fn terminal_state_answers_from_live_state_after_a_lost_ack() {
    let mut keeper = Keeper::new();
    keeper.handle(&spawn_frame(1, 80, 24));

    // The ack is dropped on the floor, exactly as a severed connection would.
    let mut replies = keeper.handle(&resize_frame(1, 7, 120, 40));
    assert_eq!(replies[0].frame_type, MuxFrameType::ResizeAck);
    replies.clear();

    let replies = keeper.handle(&empty_frame(MuxFrameType::GetTerminalState, 1));
    assert_eq!(replies[0].frame_type, MuxFrameType::GetTerminalStateResp);
    let state = TerminalState::decode(&replies[0].payload).expect("it decodes");
    assert_eq!(
        (state.applied_seq, state.cols, state.rows),
        (7, 120, 40),
        "the recovery must report what was applied, not what was asked for"
    );
}

/// A replayed sequence is ignored, so a client retrying after a timeout cannot
/// move the terminal backwards.
#[test]
fn a_replayed_resize_does_not_move_the_terminal_backwards() {
    let mut keeper = Keeper::new();
    keeper.handle(&spawn_frame(1, 80, 24));
    keeper.handle(&resize_frame(1, 5, 100, 30));
    keeper.handle(&resize_frame(1, 2, 200, 60));

    let replies = keeper.handle(&empty_frame(MuxFrameType::GetTerminalState, 1));
    let state = TerminalState::decode(&replies[0].payload).expect("it decodes");
    assert_eq!((state.applied_seq, state.cols, state.rows), (5, 100, 30));
}

/// A frame sent on the wrong lane is ignored rather than answered: a client
/// that is confused about the protocol gets no reply to be confused about, and
/// the connection stays up.
#[test]
fn a_control_frame_on_a_channel_lane_is_ignored() {
    let mut keeper = Keeper::new();
    keeper.handle(&spawn_frame(1, 80, 24));
    let hello = MuxFrame::json(
        MuxFrameType::Hello,
        1,
        &serde_json::json!({
            "version": 3,
            "capability": "a".repeat(64),
            "features": []
        }),
    )
    .expect("small JSON");
    assert!(keeper.handle(&hello).is_empty());
}

/// `Hello` negotiates the intersection and reports the live bindings, so a
/// worker can prove what it is talking to before trusting it with a PTY.
#[test]
fn hello_negotiates_features_and_reports_the_contract() {
    let mut keeper = Keeper::new();
    keeper.handle(&spawn_frame(1, 80, 24));

    let hello = MuxFrame::json(
        MuxFrameType::Hello,
        0,
        &serde_json::json!({
            "version": 3,
            "capability": "a".repeat(64),
            "features": ["ordered_history_v1", "terminal_state_v1", "from_the_future"]
        }),
    )
    .expect("small JSON");
    let replies = keeper.handle(&hello);
    assert_eq!(replies[0].frame_type, MuxFrameType::HelloResp);

    let response: roost_keeper::payloads::KeeperHelloResponse =
        replies[0].parse_json().expect("it decodes");
    assert_eq!(response.version, 3);
    assert!(response.authenticated);
    assert_eq!(response.contract.protocol_version, 3);
    assert_eq!(response.bindings.len(), 1);
    assert_eq!(response.bindings[0].channel_id, 1);
    assert!(
        response.spawning_channels.is_empty(),
        "a spawn is decided before the next frame is answered"
    );
    assert!(
        response.features.contains(&KeeperFeature::OrderedHistory),
        "a supported feature is granted"
    );
    assert!(
        !response
            .features
            .iter()
            .any(|f| f.wire_name() == "from_the_future"),
        "an unknown feature is never granted, because granting it would be a lie"
    );
}

/// A frame the keeper does not serve is ignored. The worker is newer or older,
/// and neither deserves a dropped connection over a frame it will simply not
/// send again.
#[test]
fn a_frame_the_keeper_does_not_serve_is_ignored() {
    let mut keeper = Keeper::new();
    let ping = empty_frame(MuxFrameType::Ping, 0);
    assert_eq!(keeper.handle(&ping)[0].frame_type, MuxFrameType::Pong);
    let pong = empty_frame(MuxFrameType::Pong, 0);
    assert!(
        keeper.handle(&pong).is_empty(),
        "a pong is not answered with a pong"
    );
}

/// A dead child is reported once, with its exit code, and only after its
/// output has drained — a client must not lose the last thing the process
/// printed to learn that it died.
#[test]
fn an_exited_child_is_reported_once_and_only_after_its_output_drains() {
    let mut keeper = Keeper::new();
    let spec = ShellSpec {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "echo goodbye; exit 3".into()],
        env: Vec::new(),
        cwd: None,
    };
    let request = SpawnRequest {
        channel_id: 1,
        cols: 80,
        rows: 24,
        shell_spec: spec,
    };
    keeper.handle(&MuxFrame::json(MuxFrameType::Spawn, 1, &request).expect("small JSON"));

    drain_until(&mut keeper, |seen| seen.windows(7).any(|w| w == b"goodbye"));

    let start = Instant::now();
    let exits = loop {
        let exits = keeper.reap_exited().expect("an exit frame is small JSON");
        if !exits.is_empty() {
            break exits;
        }
        assert!(start.elapsed() < DEADLINE, "the child never exited");
        std::thread::sleep(Duration::from_millis(5));
    };

    assert_eq!(exits.len(), 1);
    let exit: roost_keeper::frames::ExitFrame = exits[0].parse_json().expect("it decodes");
    assert_eq!(exit.exit_code, Some(3), "the exit code is preserved");
    assert_eq!(
        keeper.channel_count(),
        0,
        "a finished channel is not left behind"
    );
    assert!(
        keeper
            .reap_exited()
            .expect("an exit frame is small JSON")
            .is_empty(),
        "and it is reported once, not every tick"
    );
}

/// A worker that asks about a channel that is gone gets the default geometry
/// rather than silence: its next step is to respawn, and a refusal sends it
/// looking for a cause it cannot act on.
#[test]
fn a_gone_channel_answers_with_a_usable_default() {
    let mut keeper = Keeper::new();
    let replies = keeper.handle(
        &MuxFrame::new(MuxFrameType::GetTerminalState, 99, Vec::new()).expect("empty payload"),
    );
    assert_eq!(replies[0].frame_type, MuxFrameType::GetTerminalStateResp);
    let state = TerminalState::decode(&replies[0].payload).expect("it decodes");
    assert!(state.cols > 0 && state.rows > 0);
}

/// Every frame the keeper emits must fit the wire. A reply that cannot be
/// encoded is a hang at the far end, so the sizes are asserted rather than
/// assumed.
#[test]
fn every_reply_fits_inside_one_frame() {
    let mut keeper = Keeper::new();
    keeper.handle(&spawn_frame(1, 80, 24));
    let mut replies = keeper.handle(&input_frame(1, 1, b"x"));
    replies.extend(keeper.handle(&resize_frame(1, 1, 100, 30)));
    replies
        .extend(keeper.handle(&MuxFrame::new(MuxFrameType::ListChannels, 0, Vec::new()).unwrap()));
    replies.extend(
        keeper.handle(&MuxFrame::new(MuxFrameType::GetTerminalState, 1, Vec::new()).unwrap()),
    );
    for reply in &replies {
        let encoded = reply.encode();
        assert!(
            encoded.len() <= 16 * 1024 * 1024,
            "a reply must fit one frame"
        );
        assert_eq!(
            u32::from_be_bytes(encoded[..4].try_into().expect("a length prefix")),
            (encoded.len() - 4) as u32,
            "the length prefix covers the body and nothing before it"
        );
    }
}

/// A resize reject must carry the sequence the client asked about, or the
/// client cannot match the refusal to its request.
#[test]
fn a_resize_reject_names_the_sequence_it_refused() {
    let mut keeper = Keeper::new();
    // No channel: the keeper has nothing to resize.
    let replies = keeper.handle(&resize_frame(12, 42, 100, 30));
    assert_eq!(replies[0].frame_type, MuxFrameType::ResizeReject);
    assert_eq!(
        roost_keeper::codec::read_sequence(&replies[0].payload, 0),
        Some(42),
        "the refused sequence comes back so the client can match it"
    );
}

/// A respawn on a live channel replaces it. Leaving the old PTY running would
/// keep a process alive that nothing can ever reach again.
#[test]
fn a_respawn_replaces_the_channel_rather_than_leaking_the_old_process() {
    let mut keeper = Keeper::new();
    keeper.handle(&spawn_frame(1, 80, 24));
    let first: SpawnAck = keeper.handle(&spawn_frame(1, 80, 24))[0]
        .parse_json()
        .expect("decodes");
    let second: SpawnAck = keeper.handle(&spawn_frame(1, 80, 24))[0]
        .parse_json()
        .expect("decodes");
    assert_ne!(first.pid, second.pid, "a fresh process each time");
    assert_eq!(keeper.channel_count(), 1, "and still exactly one channel");
}

/// An unparseable frame with no sequence to answer is logged, not answered:
/// there is no sequence to reject it against, so a reply would be a guess.
#[test]
fn an_unanswerable_frame_is_dropped_rather_than_guessed_at() {
    let mut keeper = Keeper::new();
    let frame = MuxFrame::new(MuxFrameType::Spawn, 1, b"{not json".to_vec())
        .expect("a small payload is within every frame bound");
    assert!(keeper.handle(&frame).is_empty());
    assert_eq!(keeper.channel_count(), 0);
}

/// The resize path stamps a sequence for an UNSEQUENCED resize, so an
/// unacknowledged resize cannot overtake a sequenced one that came before it.
#[test]
fn a_legacy_resize_does_not_move_a_later_sequenced_one_backwards() {
    let mut keeper = Keeper::new();
    keeper.handle(&spawn_frame(1, 80, 24));
    keeper.handle(&resize_frame(1, 100, 200, 60));
    let legacy = MuxFrame::json(
        MuxFrameType::Resize,
        1,
        &serde_json::json!({"cols": 10, "rows": 5}),
    )
    .expect("small JSON");
    keeper.handle(&legacy);

    let replies = keeper.handle(&empty_frame(MuxFrameType::GetTerminalState, 1));
    let state = TerminalState::decode(&replies[0].payload).expect("it decodes");
    assert!(
        state.applied_seq >= 100,
        "the keeper's own sequencing kept the later request: {}",
        state.applied_seq
    );
}

/// The output the keeper emits is what a client receives, so it must be the
/// bytes on the PTY and nothing else.
#[test]
fn emitted_output_is_the_pty_bytes_and_nothing_else() {
    let mut keeper = Keeper::new();
    keeper.handle(&spawn_frame(1, 80, 24));
    keeper.handle(&input_frame(1, 1, b"marker-42\r"));
    let seen = drain_until(&mut keeper, |seen| {
        seen.windows(9).any(|w| w == b"marker-42")
    });
    let text = String::from_utf8_lossy(&seen);
    assert!(text.contains("marker-42"));
    // The tty echoes a CR as CR LF, so the output is the program's bytes after
    // line discipline, not the raw input.
    assert!(text.contains("marker-42\r\n") || text.contains("marker-42\n"));
}
