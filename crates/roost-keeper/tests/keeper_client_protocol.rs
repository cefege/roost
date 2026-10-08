#![cfg(unix)]
//! The keeper client's request/response shapes: what each call asserts about
//! the answer, and what a refusal must name. Split from the connection tests
//! because a failure in one is a protocol contract rather than a question of
//! whether the client can reach a keeper at all.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::time::{Duration, Instant};

use roost_keeper::client::{KeeperClient, connect};
use roost_keeper::payloads::{PtyInRejectReason, PtyInResult};
use support::daemon::{DEADLINE, Keeper, TempDir};
use support::echo;

/// Send one sequenced input, then read its answer off the event stream, where
/// the keeper's input lane writes it once the batch has settled.
fn answered(client: &KeeperClient, channel_id: u16, input_seq: u64, bytes: &[u8]) -> PtyInResult {
    client
        .send_input_request(channel_id, input_seq, bytes)
        .expect("the request is written");
    let deadline = Instant::now() + DEADLINE;
    while Instant::now() < deadline {
        if let Some(frame) = client.next_event(Duration::from_millis(50))
            && frame.channel_id == channel_id
            && let Some(result) = PtyInResult::decode(frame.frame_type, &frame.payload)
        {
            return result;
        }
    }
    panic!("input {input_seq} on channel {channel_id} was never answered");
}

/// property a client needs to decide between a safe retry and a duplicate.
#[test]
fn a_sequenced_write_reports_what_reached_the_pty() {
    let temp = TempDir::new("sequenced");
    let _keeper = Keeper::start(&temp);

    let client = connect(&temp.endpoint()).expect("a handshake");
    client
        .spawn(1, echo(), 80, 24)
        .expect("the spawn is acknowledged");

    let result = answered(&client, 1, 1, b"sequenced\r");
    match result {
        PtyInResult::Ack { input_seq, written } => {
            assert_eq!(input_seq, 1, "the answer names the request");
            assert_eq!(written, 10);
        }
        other => panic!("a small write to an idle PTY completes, got {other:?}"),
    }
}

/// A sequenced write to a channel that does not exist is REJECTED rather than
/// dropped, and the rejection names the sequence so the client can match it.
#[test]
fn a_sequenced_write_to_an_unknown_channel_is_rejected() {
    let temp = TempDir::new("nosuchchan");
    let _keeper = Keeper::start(&temp);

    let client = connect(&temp.endpoint()).expect("a handshake");
    let result = answered(&client, 99, 7, b"nowhere\r");

    match result {
        PtyInResult::Reject { input_seq, reason } => {
            assert_eq!(input_seq, 7, "the refusal names the request");
            assert_eq!(reason, PtyInRejectReason::NoSuchChannel);
        }
        other => panic!("a write to a channel that does not exist is rejected, got {other:?}"),
    }
}

/// The channel list is how a resuming worker finds what survived it.
#[test]
fn the_channel_list_names_the_surviving_channels() {
    let temp = TempDir::new("list");
    let _keeper = Keeper::start(&temp);

    let client = connect(&temp.endpoint()).expect("a handshake");
    client
        .spawn(3, echo(), 80, 24)
        .expect("the spawn is acknowledged");
    client
        .spawn(9, echo(), 80, 24)
        .expect("the second spawn is acknowledged");

    let listed = client.list_channels().expect("the list comes back");
    let ids: Vec<u16> = listed.channels.iter().map(|c| c.channel_id).collect();
    assert_eq!(ids, vec![3, 9], "sorted, so two workers see the same order");
}

/// A PTY that outlives its client is the keeper's reason for existing, and the
/// client can see it from a second connection.
#[test]
fn a_second_client_finds_the_channel_the_first_left() {
    let temp = TempDir::new("resume");
    let _keeper = Keeper::start(&temp);

    {
        let first = connect(&temp.endpoint()).expect("a handshake");
        first
            .spawn(4, echo(), 80, 24)
            .expect("the spawn is acknowledged");
    }

    // The keeper serves one connection at a time, so it needs a moment to
    // notice the first one is gone. The client's connect retry covers exactly
    // this, which is why the test does not sleep a guessed interval.
    let second = connect(&temp.endpoint()).expect("a handshake once the keeper is free");
    let listed = second.list_channels().expect("the list comes back");
    assert_eq!(listed.channels.len(), 1);
    assert_eq!(listed.channels[0].channel_id, 4);
}
