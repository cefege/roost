//! F6: a resize answers with what the keeper DID, and every reason the keeper
//! can refuse with survives the trip.
//!
//! Before this, `resize()` returned `()` or one I/O string: the acknowledged
//! sequence and geometry were decoded and dropped, and nine refusal reasons
//! became "the keeper refused the resize of channel N". A caller that read that
//! as a transient failure retried a resize the keeper had already declined, and
//! a caller that read the absence of an error as success believed the PTY was
//! at a geometry nothing had ever proved.
//!
//! Every expectation is the daemon's, cited by line. The discriminating case
//! throughout is a STALE sequence: `apply_resize` (`pty_channel.rs:200`) keeps
//! the geometry it already has and returns the sequence it already applied, so
//! the acknowledgement for a 200x60 request is 132x43. A client that answered
//! from its own request cannot produce that, which is why asserting the
//! requested geometry would test nothing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::time::{Duration, Instant};

use roost_keeper::client::KeeperClient;
use roost_keeper::client::connect;
use roost_keeper::client_resize::{
    RESIZE_ACK_TIMEOUT, ResizeOutcome, ResizeRejectReason, ResizeUnknownReason,
};
use support::daemon::{Keeper, TempDir};
use support::echo;

fn client_with_channel(temp: &TempDir) -> (Keeper, KeeperClient) {
    let keeper = Keeper::start(temp);
    let client = connect(keeper.socket()).expect("a handshake");
    client.spawn(1, echo(), 80, 24).expect("a spawn");
    (keeper, client)
}

/// F6 — the answer is the APPLIED geometry, not the requested one.
///
/// The daemon builds the acknowledgement from the PTY's own state
/// (`keeper_ops.rs:225-233`), and for a stale sequence that state is the
/// PREVIOUS one. This is the specific lie the defect existed to remove: a caller
/// told 200x60 would drive a core sized for a terminal that is 132x43.
#[test]
fn a_stale_resize_is_acknowledged_with_the_geometry_the_keeper_already_had() {
    let temp = TempDir::new("stale");
    let (_keeper, client) = client_with_channel(&temp);

    assert_eq!(
        client.resize(1, 9, 132, 43),
        ResizeOutcome::Applied {
            seq: 9,
            cols: 132,
            rows: 43
        },
        "a fresh sequence reports what it applied"
    );
    assert_eq!(
        client.resize(1, 4, 200, 60),
        ResizeOutcome::Applied {
            seq: 9,
            cols: 132,
            rows: 43
        },
        "a stale sequence is acknowledged with the geometry the PTY is at \
         (pty_channel.rs:200), NOT the 200x60 that was asked for"
    );
}

/// F6 — a refusal keeps the sequence it is refusing AND the reason.
///
/// The daemon names the sequence so the caller can match the refusal to its own
/// request (`keeper_ops.rs:213-217`, a zero there would be a hang wearing a
/// different hat) and codes the reason 1, `channel_missing`, from v2's table
/// (`protocol-terminal.ts:52`). A channel this keeper does not have is the one
/// refusal the daemon can be made to produce deterministically.
#[test]
fn a_refused_resize_names_the_sequence_and_the_reason() {
    let temp = TempDir::new("refused");
    let keeper = Keeper::start(&temp);
    let client = connect(keeper.socket()).expect("a handshake");

    assert_eq!(
        client.resize(9_999, 42, 80, 24),
        ResizeOutcome::Refused {
            seq: 42,
            reason: ResizeRejectReason::ChannelMissing,
        },
        "the refusal is an answer with a matchable sequence and a reason, not \
         an I/O string; `channel_missing` is code 1 (protocol-terminal.ts:52)"
    );
}

/// F6 — the third outcome, which the old signature could not express at all.
///
/// A resize whose answer never arrives is NOT a refusal: the request may well
/// have landed, so the caller must ask `resize_status` rather than either
/// resending the sequence or assuming the old geometry held. A daemon that is
/// gone produces it, and it must arrive without spending the acknowledgement
/// window doing nothing.
#[test]
fn a_resize_the_keeper_never_answers_is_unknown_and_bounded() {
    let temp = TempDir::new("unknown");
    let (keeper, client) = client_with_channel(&temp);
    // The daemon goes away mid-request: the write may fail, or land on a socket
    // whose peer is gone. Both are the same verdict, and neither may be a
    // refusal, because a refusal says nothing was written.
    drop(keeper);

    let start = Instant::now();
    let outcome = client.resize(1, 5, 80, 24);
    let waited = start.elapsed();
    assert_eq!(
        outcome,
        ResizeOutcome::Unknown {
            seq: 5,
            reason: ResizeUnknownReason::Disconnected,
        },
        "an unanswered resize is Unknown, and the sequence is the caller's own \
         because nothing named one"
    );
    assert!(
        outcome.is_unknown(),
        "and the caller can tell it apart from a refusal without matching on text"
    );
    assert!(
        waited < RESIZE_ACK_TIMEOUT,
        "a departed keeper is not a wedged one; this took {waited:?} of a \
         {RESIZE_ACK_TIMEOUT:?} window"
    );
    assert!(
        waited < Duration::from_secs(2),
        "and it must not have waited out the window to learn it; it took {waited:?}"
    );
}

/// F6 — the nine refusal reasons, which this daemon can only be made to emit
/// one of.
///
/// Byte 4 of a resize refusal is `resize_error`, and byte 2 is `channel_exited`.
/// `PtyInRejectReason` is the INPUT vocabulary on the same shape: code 4 there
/// is `ChildExited`. A client that reused it type-checked perfectly and drove
/// the wrong recovery for eight of the nine reasons, so the codes are pinned
/// here against v2's table rather than against the crate's nearest enum.
#[test]
fn the_resize_refusal_codes_are_v2s_nine() {
    let table = [
        (ResizeRejectReason::ChannelMissing, 1, "channel_missing"),
        (ResizeRejectReason::ChannelExited, 2, "channel_exited"),
        (ResizeRejectReason::TerminalMissing, 3, "terminal_missing"),
        (ResizeRejectReason::ResizeError, 4, "resize_error"),
        (ResizeRejectReason::StaleSequence, 5, "stale_sequence"),
        (ResizeRejectReason::UnknownSequence, 6, "unknown_sequence"),
        (ResizeRejectReason::InvalidRequest, 7, "invalid_request"),
        (ResizeRejectReason::Unsupported, 8, "unsupported"),
        (ResizeRejectReason::Disconnected, 9, "disconnected"),
    ];
    for (reason, code, name) in table {
        assert_eq!(
            reason.code(),
            code,
            "{name} is code {code} on the wire (protocol-terminal.ts:50-60)"
        );
        assert_eq!(
            ResizeRejectReason::from_code(code),
            Some(reason),
            "and a keeper's byte {code} decodes back to {name}"
        );
    }
    assert_eq!(
        ResizeRejectReason::from_code(10),
        None,
        "a code from a newer keeper is refused, not guessed: every reason here \
         drives a different recovery and a wrong guess is a destructive one"
    );
}
