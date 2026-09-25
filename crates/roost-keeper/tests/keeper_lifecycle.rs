//! The keeper's channel lifecycle: spawn, discovery, conditional shutdown and
//! exit. Split from the request/response tests because these frames decide
//! whether a PTY exists at all, and a mistake in one is a leaked process
//! rather than a wrong answer.
//!
//! The contract is `protocol/spec/keeper.md`.

mod support;

use std::time::{Duration, Instant};

use roost_keeper::codec::{MuxFrame, MuxFrameType};
use roost_keeper::frames::{SpawnAck, SpawnRequest};
use roost_keeper::keeper::Keeper;
use support::{DEADLINE, echo, empty_frame, spawn_frame};

/// A spawn is answered with the child's pid, and the channel becomes real.
#[test]
fn a_spawn_is_acknowledged_with_a_real_process() {
    let mut keeper = Keeper::new();
    let replies = keeper.handle(&spawn_frame(7, 80, 24));
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].frame_type, MuxFrameType::SpawnAck);
    assert_eq!(replies[0].channel_id, 7);
    let ack: SpawnAck = replies[0].parse_json().expect("the ack decodes");
    assert!(ack.pid > 0, "a spawned PTY has a process, not pid 0");
    assert_eq!(keeper.channel_count(), 1);
}

/// A spawn that cannot be honoured must SAY SO. The v2 incident this exists
/// for: a degraded keeper accepts Spawn and never replies, and the worker's RPC
/// hangs forever with no trail.
#[test]
fn a_spawn_that_cannot_be_honoured_answers_instead_of_hanging() {
    let mut keeper = Keeper::new();
    let bad = SpawnRequest {
        channel_id: 3,
        cols: 0,
        rows: 24,
        shell_spec: echo(),
    };
    let frame = MuxFrame::json(MuxFrameType::Spawn, 3, &bad).expect("small JSON");
    let replies = keeper.handle(&frame);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].frame_type, MuxFrameType::SpawnErr);
    assert_eq!(
        keeper.channel_count(),
        0,
        "a refused spawn leaves no channel"
    );
}

/// `ListChannels` is how a fresh worker discovers what survived it, so the
/// answer has to be sorted: two workers polling one keeper must not see
/// different orders.
#[test]
fn channels_are_listed_in_a_stable_order() {
    let mut keeper = Keeper::new();
    for channel_id in [9u16, 2, 5, 1] {
        keeper.handle(&spawn_frame(channel_id, 80, 24));
    }
    let replies = keeper.handle(&MuxFrame::new(MuxFrameType::ListChannels, 0, Vec::new()).unwrap());
    assert_eq!(replies.len(), 1);
    let listed: Vec<u16> = replies[0]
        .parse_json::<roost_keeper::frames::ListChannelsResp>()
        .expect("the list decodes")
        .channels
        .into_iter()
        .map(|binding| binding.channel_id)
        .collect();
    assert_eq!(listed, vec![1, 2, 5, 9], "sorted, not HashMap order");
    assert_eq!(
        keeper.handle(&replies[0]).len(),
        0,
        "a response is not a request"
    );
}

/// `ShutdownIfEmpty` is the automatic boot-replacement path. The check and the
/// answer must be one operation, or a keeper handed a new PTY can retire itself
/// out from under the channel it was just given.
#[test]
fn a_conditional_shutdown_refuses_while_a_channel_is_live() {
    let mut keeper = Keeper::new();
    let request = empty_frame(MuxFrameType::ShutdownIfEmpty, 0);

    let replies = keeper.handle(&request);
    assert_eq!(replies[0].frame_type, MuxFrameType::ShutdownIfEmptyAck);
    assert!(keeper.is_empty());

    keeper.handle(&spawn_frame(4, 80, 24));
    let replies = keeper.handle(&request);
    assert_eq!(
        replies[0].frame_type,
        MuxFrameType::ShutdownIfEmptyReject,
        "a live channel must stop the keeper retiring"
    );

    keeper.handle(&MuxFrame::new(MuxFrameType::KillChild, 4, Vec::new()).unwrap());
    let start = Instant::now();
    while keeper.channel_count() > 0 {
        keeper.reap_exited();
        assert!(start.elapsed() < DEADLINE, "a killed child never exited");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        keeper.handle(&request)[0].frame_type,
        MuxFrameType::ShutdownIfEmptyAck
    );
}

/// The unconditional `Shutdown` is the deliberate offline maintenance path, so
/// it does not consult the channel table at all.
#[test]
fn an_unconditional_shutdown_answers_regardless_of_channels() {
    let mut keeper = Keeper::new();
    keeper.handle(&spawn_frame(1, 80, 24));
    let replies = keeper.handle(&MuxFrame::new(MuxFrameType::Shutdown, 0, Vec::new()).unwrap());
    assert_eq!(replies[0].frame_type, MuxFrameType::ShutdownAck);
}
