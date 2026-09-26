//! The old-coordinator raw lane and the cadence's echo promotions. Both are
//! about bounded memory on a path that runs while a terminal is flooding.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "session_emit_support/mod.rs"]
mod support;

use roost_worker::session::cell_scheduler::MAX_PENDING_INPUT_ECHO_PROMOTIONS;
use roost_worker::session::emit::CellEmitter;
use roost_worker::session::raw_metadata::{
    RAW_METADATA_AGGREGATE_CAP_BYTES, RAW_METADATA_CHANNEL_CAP_BYTES,
    RAW_METADATA_DISPATCH_FRAME_BUDGET, RawMetadataStage,
};

use support::{Answer, RecordFixture, RecordingSink, channel};

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
    assert!(
        stage.drain().is_empty(),
        "a negotiated link staged raw bytes"
    );
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

/// A drain hands over one dispatch budget, oldest channel first, so one busy
/// session cannot reorder another session's bytes or starve the cells behind.
#[test]
fn a_drain_is_bounded_and_keeps_each_channels_own_order() {
    let mut stage = RawMetadataStage::default();
    for chunk in 0..RAW_METADATA_DISPATCH_FRAME_BUDGET + 5 {
        stage.stage(channel(33), chunk as u64, format!("a{chunk}").as_bytes());
    }
    for chunk in 0..3 {
        stage.stage(channel(34), chunk as u64, format!("b{chunk}").as_bytes());
    }
    let first = stage.drain();
    assert_eq!(
        first.len(),
        RAW_METADATA_DISPATCH_FRAME_BUDGET,
        "one dispatch took the whole queue and starves the lanes behind it"
    );
    assert!(
        first.iter().all(|frame| frame.channel_id == channel(33)),
        "the drain reached past the first channel with work"
    );
    assert!(
        first
            .windows(2)
            .all(|pair| pair[0].end_seq < pair[1].end_seq),
        "a channel's own frames came back out of order"
    );
    assert!(
        stage.staged_bytes() > 0,
        "the queue was emptied by one dispatch"
    );
}

/// An echo promotion is taken once and is bounded. Consuming membership instead
/// of a count would promote only the first keystroke of a burst and make every
/// later one wait out the coalesce window.
#[test]
fn an_echo_promotion_is_taken_once_per_queued_keystroke_and_is_bounded() {
    let mut emitter = CellEmitter::new();
    for _ in 0..MAX_PENDING_INPUT_ECHO_PROMOTIONS + 4 {
        emitter.note_input_echo(channel(35));
    }
    let mut taken = 0;
    while emitter.consume_input_echo_promotion(channel(35)) {
        taken += 1;
        assert!(taken <= MAX_PENDING_INPUT_ECHO_PROMOTIONS as usize);
    }
    assert_eq!(taken, MAX_PENDING_INPUT_ECHO_PROMOTIONS as usize);
    assert!(
        !emitter.consume_input_echo_promotion(channel(35)),
        "a promotion was taken past the bound"
    );
    assert!(
        !emitter.consume_input_echo_promotion(channel(36)),
        "a channel with no queued keystroke handed one out"
    );
}

/// A promotion is taken by the chunk that carries its echo, not by the next
/// keystroke: a queued promotion that is never consumed must not make every
/// later chunk look promoted.
#[test]
fn a_promotion_is_taken_by_the_chunk_that_carries_the_echo() {
    let fixture = RecordFixture::new();
    let mut record = fixture.record(channel(37), 80, 24);
    let sink = RecordingSink::new("coord", Answer::Sent);
    let mut emitter = CellEmitter::new();
    emitter.register_sink(sink.clone());
    emitter.install_stream(&mut record, "stream-37");
    emitter.emit_cell_frame(&mut record, true, 1_000);
    emitter.note_input_echo(channel(37));

    let first = emitter.ingest_pty_chunk(&mut record, b"k", 1_010);
    assert_eq!(
        first,
        roost_worker::session::emit::IngestOutcome::Accepted {
            end_seq: 1,
            input_echo: true
        },
        "the echo chunk did not consume its promotion"
    );
    let second = emitter.ingest_pty_chunk(&mut record, b"l", 1_020);
    assert_eq!(
        second,
        roost_worker::session::emit::IngestOutcome::Accepted {
            end_seq: 2,
            input_echo: false
        },
        "a second keystroke borrowed the first one's promotion"
    );
}
