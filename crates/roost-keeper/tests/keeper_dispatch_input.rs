//! The keeper dispatcher's two input frames, `PtyIn` and `PtyInRequest`, driven
//! with no listener and no socket. An acknowledged batch is answered by its
//! channel's input lane (read here through `support::tap_results`); a request the
//! lane never sees — no such channel, an unreadable payload — is refused by the
//! dispatcher itself, never dropped. The contract is `protocol/spec/keeper.md`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use roost_keeper::codec::{MuxFrame, MuxFrameType, write_sequence};
use roost_keeper::keeper::Keeper;
use roost_keeper::payloads::{PtyInRejectReason, PtyInResult};
use support::{drain_until, input_frame, next_result, spawn_frame, tap_results};

/// Acknowledged input is the whole point of the sequenced form: a write is
/// either complete, safely retryable, or ambiguous, and the client must be able
/// to tell which.
#[test]
fn acknowledged_input_reports_exactly_what_reached_the_pty() {
    let mut keeper = Keeper::new();
    let results = tap_results(&mut keeper);
    keeper.handle(&spawn_frame(1, 80, 24));

    assert!(
        keeper.handle(&input_frame(1, 1, b"hello\r")).is_empty(),
        "the lane answers"
    );
    let answer = next_result(&results);
    assert_eq!(
        PtyInResult::decode(answer.frame_type, &answer.payload),
        Some(PtyInResult::Ack {
            input_seq: 1,
            written: 6
        })
    );
    drain_until(&mut keeper, |seen| seen.windows(5).any(|w| w == b"hello"));
}

/// Input for a channel that does not exist is refused, not dropped. A dropped
/// frame leaves the worker waiting for an acknowledgement that never comes.
#[test]
fn input_for_an_unknown_channel_is_refused_rather_than_dropped() {
    let mut keeper = Keeper::new();
    let replies = keeper.handle(&input_frame(42, 9, b"lost"));
    assert_eq!(replies.len(), 1, "a waiting client must get an answer");
    assert_eq!(replies[0].frame_type, MuxFrameType::PtyInReject);
    assert_eq!(
        PtyInResult::decode(MuxFrameType::PtyInReject, &replies[0].payload),
        Some(PtyInResult::Reject {
            input_seq: 9,
            reason: PtyInRejectReason::NoSuchChannel
        })
    );
}

/// A malformed payload is still answered, for the same reason: a worker with
/// no reply is the silent-hang class, and silence is not an answer.
#[test]
fn an_unreadable_input_payload_is_answered_not_dropped() {
    let mut keeper = Keeper::new();
    let frame = MuxFrame::new(MuxFrameType::PtyInRequest, 1, vec![1, 2, 3]).unwrap();
    let replies = keeper.handle(&frame);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].frame_type, MuxFrameType::PtyInReject);
}

/// A legacy unacknowledged write is still honoured. It simply cannot say what
/// it did, which is the entire reason the sequenced form exists.
#[test]
fn a_legacy_unacknowledged_write_still_reaches_the_pty() {
    let mut keeper = Keeper::new();
    keeper.handle(&spawn_frame(1, 80, 24));
    let legacy = MuxFrame::new(MuxFrameType::PtyIn, 1, b"legacy\r".to_vec())
        .expect("a small payload is within every frame bound");
    assert!(
        keeper.handle(&legacy).is_empty(),
        "an unacknowledged write has no reply"
    );
    drain_until(&mut keeper, |seen| seen.windows(6).any(|w| w == b"legacy"));
}

/// A reject payload names the sequence it is refusing. A client that cannot
/// match a refusal to its request retries forever.
#[test]
fn a_malformed_input_reject_is_still_well_formed() {
    let mut keeper = Keeper::new();
    let frame = MuxFrame::new(MuxFrameType::PtyInRequest, 1, vec![0, 1]).unwrap();
    let replies = keeper.handle(&frame);
    let encoded = replies[0].encode();
    assert!(encoded.len() > 4, "a reject carries a payload");
    assert_eq!(replies[0].frame_type, MuxFrameType::PtyInReject);
    // The payload is the fixed-width reject form, whatever the sequence.
    assert_eq!(replies[0].payload.len(), 13);
}

/// A helper the other tests use, kept honest by the compiler: the acknowledged
/// write must be a complete one for the byte counts asserted above to mean
/// anything.
#[test]
fn a_write_to_a_live_pty_is_complete() {
    let mut keeper = Keeper::new();
    let results = tap_results(&mut keeper);
    keeper.handle(&spawn_frame(1, 80, 24));
    let mut payload = Vec::new();
    write_sequence(&mut payload, 1);
    payload.extend_from_slice(b"x");
    let frame = MuxFrame::new(MuxFrameType::PtyInRequest, 1, payload).unwrap();
    assert!(
        keeper.handle(&frame).is_empty(),
        "the lane answers, not the dispatcher"
    );
    assert_eq!(
        next_result(&results).frame_type,
        MuxFrameType::PtyInAck,
        "a one-byte write to an idle PTY completes"
    );
}
