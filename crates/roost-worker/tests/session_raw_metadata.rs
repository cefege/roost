//! The old-coordinator raw lane and the cadence's echo promotions. Both are
//! about bounded memory on a path that runs while a terminal is flooding.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "session_emit_support/mod.rs"]
mod support;

use roost_worker::session::cell_scheduler::INPUT_ECHO_WINDOW_MS;
use roost_worker::session::emit::CellEmitter;
use roost_worker::session::raw_metadata::{
    RAW_METADATA_AGGREGATE_CAP_BYTES, RAW_METADATA_CHANNEL_CAP_BYTES,
    RAW_METADATA_DISPATCH_FRAME_BUDGET, RawMetadataStage, RawSend,
};

use support::{Answer, RecordFixture, RecordingSink, channel, stream_id};

/// A coordinator that negotiated the semantic lane never sees the raw one, and
/// a lane that filled up before the negotiation arrived is emptied rather than
/// shipped: those bytes were staged under the old contract.
#[test]
fn a_negotiated_link_stages_no_raw_bytes_at_all() {
    let mut stage = RawMetadataStage::default();
    stage.stage(channel(31), 10, b"raw pty bytes");
    assert_eq!(stage.staged_bytes(), 13, "bytes were not staged");

    stage.set_semantic_metadata_negotiated(true);
    assert!(
        stage.staged_bytes() == 0,
        "the negotiation left bytes staged"
    );
    stage.stage(channel(31), 23, b"more raw pty bytes");
    let mut sent = 0;
    stage.dispatch(
        &|_| true,
        &mut |_| {
            sent += 1;
            RawSend::Accepted
        },
        std::time::Instant::now(),
    );
    assert_eq!(sent, 0, "a negotiated link staged raw bytes");
}

/// A single chatty session must not be able to take the whole lane, so the
/// per-channel cap refuses before the aggregate one is even consulted.
#[test]
fn one_channel_cannot_take_the_whole_raw_lane() {
    let mut stage = RawMetadataStage::default();
    let flood = vec![b'x'; RAW_METADATA_CHANNEL_CAP_BYTES + 1];
    stage.stage(channel(32), 1, &flood);
    assert_eq!(stage.staged_bytes(), 0, "an oversized frame was staged");
    assert_eq!(stage.dropped_frames(), 1);

    // A frame that fits, staged until the channel is full.
    let chunk = vec![b'y'; 64 * 1024];
    let per_channel = RAW_METADATA_CHANNEL_CAP_BYTES / chunk.len();
    for index in 0..per_channel {
        stage.stage(channel(32), index as u64, &chunk);
    }
    assert_eq!(stage.staged_bytes(), per_channel * chunk.len());
    stage.stage(channel(32), 999, &chunk);
    assert_eq!(
        stage.dropped_frames(),
        2,
        "the per-channel cap did not refuse the frame that crossed it"
    );
    assert!(stage.staged_bytes() <= RAW_METADATA_AGGREGATE_CAP_BYTES);
}

/// v2 `drainRawMetadata`: one bounded turn, round-robin by readiness, so one
/// busy session cannot starve another's scanners or reorder its own bytes.
#[test]
fn a_dispatch_is_bounded_round_robin_and_keeps_each_channels_own_order() {
    let mut stage = RawMetadataStage::default();
    for chunk in 0..RAW_METADATA_DISPATCH_FRAME_BUDGET + 5 {
        stage.stage(channel(33), chunk as u64, format!("a{chunk}").as_bytes());
    }
    for chunk in 0..3 {
        stage.stage(channel(34), chunk as u64, format!("b{chunk}").as_bytes());
    }
    let mut first = Vec::new();
    stage.dispatch(
        &|_| true,
        &mut |frame| {
            first.push(frame.clone());
            RawSend::Accepted
        },
        std::time::Instant::now(),
    );
    assert_eq!(
        first.len(),
        RAW_METADATA_DISPATCH_FRAME_BUDGET,
        "one dispatch took the whole queue and starves the lanes behind it"
    );
    let order: Vec<u32> = first
        .iter()
        .take(6)
        .map(|frame| frame.channel_id.as_u32())
        .collect();
    assert_eq!(
        order,
        vec![33, 34, 33, 34, 33, 34],
        "the dispatch was not round-robin"
    );
    for id in [33, 34] {
        let seqs: Vec<u64> = first
            .iter()
            .filter(|f| f.channel_id.as_u32() == id)
            .map(|f| f.end_seq)
            .collect();
        assert!(
            seqs.windows(2).all(|pair| pair[0] < pair[1]),
            "a channel's own frames came back out of order"
        );
    }
    assert!(
        stage.staged_bytes() > 0,
        "the queue was emptied by one dispatch"
    );
    assert_eq!(stage.channel_backlog(channel(33)).0, 8);
}

/// A frame the link refuses drops that channel's whole queue (v2
/// `dropRawMetadataQueue`), and a gone session's staging is disposed.
#[test]
fn a_refused_frame_drops_its_channels_queue_and_a_gone_session_is_disposed() {
    let mut stage = RawMetadataStage::default();
    stage.stage(channel(38), 1, b"one");
    stage.stage(channel(38), 2, b"two");
    stage.stage(channel(39), 1, b"gone");
    let mut sent = Vec::new();
    stage.dispatch(
        &|id| id.as_u32() != 39,
        &mut |frame| {
            sent.push(frame.end_seq);
            RawSend::Dropped
        },
        std::time::Instant::now(),
    );
    assert_eq!(sent, vec![1], "a refused queue kept sending");
    assert_eq!(stage.staged_bytes(), 0);
    assert_eq!(stage.channel_backlog(channel(39)), (0, 0));
}

/// The promotion is a window from the keystroke, held per channel: a channel
/// nobody typed into is never armed, and a second keystroke re-arms the window
/// from its own instant rather than stacking promotions.
#[test]
fn an_echo_window_is_per_channel_and_a_later_keystroke_rearms_it() {
    let mut emitter = CellEmitter::new();
    emitter.note_input_echo(channel(35), 1_000);
    assert!(emitter.input_echo_armed(channel(35), 1_000));
    assert!(
        !emitter.input_echo_armed(channel(36), 1_000),
        "a channel with no keystroke was armed"
    );
    let closes = 1_000 + INPUT_ECHO_WINDOW_MS;
    assert!(emitter.input_echo_armed(channel(35), closes - 1));
    assert!(
        !emitter.input_echo_armed(channel(35), closes),
        "the window outlived its bound"
    );

    emitter.note_input_echo(channel(35), 2_000);
    assert!(
        emitter.input_echo_armed(channel(35), 2_000 + INPUT_ECHO_WINDOW_MS - 1),
        "a second keystroke did not re-arm the window"
    );
}

/// Every chunk inside the window reports its promotion, and the first chunk
/// past it does not: a spinner landing ahead of the echo cannot take it, and an
/// armed window cannot make all later output look promoted.
#[test]
fn every_chunk_inside_the_echo_window_is_promoted_and_none_after_it() {
    let fixture = RecordFixture::new();
    let mut record = fixture.record(channel(37), 80, 24);
    let sink = RecordingSink::new("coord", Answer::Sent);
    let mut emitter = CellEmitter::new();
    emitter.register_sink(sink.clone());
    emitter.install_stream(&mut record, &stream_id(37));
    emitter.emit_cell_frame(&mut record, true, 1_000);
    emitter.note_input_echo(channel(37), 1_000);

    for (end_seq, at_ms) in [(1, 1_001), (2, 1_010)] {
        assert_eq!(
            emitter.ingest_pty_chunk(&mut record, b"k", at_ms),
            roost_worker::session::emit::IngestOutcome::Accepted {
                end_seq,
                input_echo: true
            },
            "a chunk inside the window at {at_ms} was not promoted"
        );
    }
    let past = 1_000 + INPUT_ECHO_WINDOW_MS + 1;
    assert_eq!(
        emitter.ingest_pty_chunk(&mut record, b"l", past),
        roost_worker::session::emit::IngestOutcome::Accepted {
            end_seq: 3,
            input_echo: false
        },
        "a chunk past the window borrowed the keystroke's promotion"
    );
}
