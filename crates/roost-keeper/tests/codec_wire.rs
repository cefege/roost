//! Wire-format conformance for the keeper codec. These assert BYTES, not
//! behaviour, because the only thing that matters here is that a Rust worker
//! and a keeper deployed from any other build agree on every octet.
//!
//! The contract is `protocol/spec/keeper.md`. A test that moved without the
//! spec changing means one of the two is now wrong.

use roost_keeper::codec::{
    CodecError, FrameDecoder, KEEPER_MAX_INPUT_BYTES, KEEPER_MAX_MUX_FRAME_BYTES,
    MUX_FRAME_HEADER_BYTES, MuxFrame, MuxFrameType, StreamEvent, read_sequence, read_u32,
    write_sequence,
};
use roost_keeper::frames::{SpawnAck, SpawnRequest};
use roost_keeper::payloads::{
    PtyInRejectReason, PtyInRequest, PtyInResult, ResizeRequest, TerminalState,
};

fn frame_events(bytes: &[u8]) -> Vec<StreamEvent> {
    FrameDecoder::new().push(bytes)
}

/// The envelope is length-prefixed big-endian, then tag, then channel, then
/// payload. Getting the prefix wrong is the one mistake that makes every other
/// field wrong, so it is asserted literally.
#[test]
fn the_envelope_is_length_tag_channel_payload() {
    let encoded = MuxFrame {
        frame_type: MuxFrameType::PtyOut,
        channel_id: 0x0201,
        payload: vec![0xaa, 0xbb],
    }
    .encode();

    assert_eq!(
        encoded,
        vec![0, 0, 0, 5, 0x21, 0x02, 0x01, 0xaa, 0xbb],
        "length covers tag + channel + payload and nothing before it"
    );
}

/// A length that exceeds the maximum is a protocol violation, not a large
/// frame. Allocating for it first is how a 5-byte message becomes an OOM.
#[test]
fn an_oversized_length_fails_instead_of_allocating() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(KEEPER_MAX_MUX_FRAME_BYTES + 1).to_be_bytes());
    bytes.extend_from_slice(&[0u8; 8]);

    let events = frame_events(&bytes);
    assert!(
        matches!(events.as_slice(), [StreamEvent::Failed(CodecError::FrameTooLarge(n))] if *n == KEEPER_MAX_MUX_FRAME_BYTES + 1)
    );
}


/// A socket read splits wherever it likes. A decoder that assumed one frame per
/// read would corrupt every large PTY burst, so the split is driven byte by
/// byte rather than reasoned about.
#[test]
fn a_frame_survives_being_delivered_one_byte_at_a_time() {
    let frames = [
        MuxFrame {
            frame_type: MuxFrameType::PtyOut,
            channel_id: 1,
            payload: b"hello".to_vec(),
        },
        MuxFrame {
            frame_type: MuxFrameType::PtyIn,
            channel_id: 1,
            payload: b"a".repeat(300),
        },
        MuxFrame {
            frame_type: MuxFrameType::Ping,
            channel_id: 0,
            payload: Vec::new(),
        },
    ];
    let stream: Vec<u8> = frames.iter().flat_map(MuxFrame::encode).collect();

    let mut decoder = FrameDecoder::new();
    let mut decoded = Vec::new();
    for byte in &stream {
        for event in decoder.push(&[*byte]) {
            if let StreamEvent::Frame {
                frame_type,
                channel_id,
                payload,
                ..
            } = event
            {
                decoded.push((frame_type, channel_id, payload));
            }
        }
    }
    assert_eq!(
        decoder.buffered_len(),
        0,
        "a whole stream leaves nothing buffered"
    );
    assert_eq!(decoded.len(), 3);
    assert_eq!(decoded[0].2, b"hello");
    assert_eq!(decoded[1].2.len(), 300);
    assert_eq!(decoded[2].0, Some(MuxFrameType::Ping));
}

/// Several frames in one read must all come out, and a trailing partial frame
/// must be held rather than dropped.
#[test]
fn several_frames_in_one_read_all_come_out() {
    let mut stream = MuxFrame {
        frame_type: MuxFrameType::PtyOut,
        channel_id: 7,
        payload: b"a".to_vec(),
    }
    .encode();
    let tail = MuxFrame {
        frame_type: MuxFrameType::PtyOut,
        channel_id: 8,
        payload: b"bb".to_vec(),
    };
    let tail_encoded = tail.encode();
    stream.extend_from_slice(&tail_encoded[..tail_encoded.len() - 1]);

    let mut decoder = FrameDecoder::new();
    let events = decoder.push(&stream);
    assert_eq!(
        events.len(),
        1,
        "the second frame is incomplete and is held"
    );
    assert_eq!(decoder.buffered_len(), tail_encoded.len() - 1);

    let events = decoder.push(&tail_encoded[tail_encoded.len() - 1..]);
    assert_eq!(events.len(), 1);
    assert_eq!(decoder.buffered_len(), 0);
}

/// A tag this build predates is not fatal — it is how a newer keeper says it
/// has a frame the client does not know. The client can only skip it safely
/// once the length has been read, so the raw tag travels with the event.
#[test]
fn an_unknown_tag_is_reported_rather_than_fatal() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&4u32.to_be_bytes());
    bytes.push(0xEF); // a tag this build does not know
    bytes.extend_from_slice(&0x0009u16.to_be_bytes());
    bytes.push(0x42);

    let events = frame_events(&bytes);
    assert!(
        matches!(events.as_slice(), [StreamEvent::Frame { frame_type: None, raw_tag: 0xEF, channel_id: 9, payload }] if payload == &[0x42])
    );
}

/// Every tag must round-trip through its wire value. A renumbered tag breaks
/// every deployed keeper at once, so the table is asserted exhaustively rather
/// than spot-checked.
#[test]
fn every_tag_round_trips_and_none_is_reused() {
    let all = [
        (MuxFrameType::Spawn, 0x10),
        (MuxFrameType::SpawnAck, 0x11),
        (MuxFrameType::SpawnErr, 0x12),
        (MuxFrameType::PtyIn, 0x20),
        (MuxFrameType::PtyOut, 0x21),
        (MuxFrameType::PtyInRequest, 0x22),
        (MuxFrameType::PtyInAck, 0x23),
        (MuxFrameType::PtyInReject, 0x24),
        (MuxFrameType::PtyInAmbiguous, 0x25),
        (MuxFrameType::Resize, 0x30),
        (MuxFrameType::KillChild, 0x31),
        (MuxFrameType::Exit, 0x32),
        (MuxFrameType::ResizeRequest, 0x33),
        (MuxFrameType::ResizeAck, 0x34),
        (MuxFrameType::ResizeReject, 0x35),
        (MuxFrameType::ResizeStatus, 0x36),
        (MuxFrameType::ListChannels, 0xE0),
        (MuxFrameType::ListChannelsResp, 0xE1),
        (MuxFrameType::Hello, 0xE2),
        (MuxFrameType::HelloResp, 0xE3),
        (MuxFrameType::GetHistory, 0xE4),
        (MuxFrameType::GetHistoryResp, 0xE5),
        (MuxFrameType::GetHistoryRecords, 0xE6),
        (MuxFrameType::GetHistoryRecordsResp, 0xE7),
        (MuxFrameType::Shutdown, 0xE8),
        (MuxFrameType::ShutdownAck, 0xE9),
        (MuxFrameType::GetTerminalState, 0xEA),
        (MuxFrameType::GetTerminalStateResp, 0xEB),
        (MuxFrameType::ShutdownIfEmpty, 0xEC),
        (MuxFrameType::ShutdownIfEmptyAck, 0xED),
        (MuxFrameType::ShutdownIfEmptyReject, 0xEE),
        (MuxFrameType::Ping, 0xF0),
        (MuxFrameType::Pong, 0xF1),
    ];

    let mut seen = std::collections::HashSet::new();
    for (frame_type, wire) in all {
        assert_eq!(
            frame_type.tag(),
            wire,
            "{frame_type:?} has the wrong wire value"
        );
        assert_eq!(MuxFrameType::from_tag(wire), Some(frame_type));
        assert!(seen.insert(wire), "tag {wire:#04x} is used twice");
    }
    assert_eq!(seen.len(), all.len(), "a tag is duplicated in the table");
    assert_eq!(MuxFrameType::from_tag(0x00), None);
    assert_eq!(MuxFrameType::from_tag(0xFF), None);
}

/// Scalars are big-endian and unaligned, and a short buffer is an error rather
/// than a panic: a truncated frame is attacker-influenced, not exceptional.
#[test]
fn scalar_readers_refuse_to_read_past_the_end() {
    let mut out = Vec::new();
    write_sequence(&mut out, 0x0102_0304_0506_0708);
    assert_eq!(&out, &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(read_sequence(&out, 0), Some(0x0102_0304_0506_0708));
    assert_eq!(read_sequence(&out, 1), None);
    assert_eq!(read_sequence(&[], 0), None);
    assert_eq!(read_u32(&[0, 0, 1, 0], 0), Some(256));
    assert_eq!(read_u32(&[0, 0, 1], 0), None);
}

/// `[input_seq:u64][bytes]`. The sequence is what makes a lost write
/// distinguishable from one that never happened, so a round trip that dropped
/// it would be a silent keystroke loss.
#[test]
fn sequenced_input_round_trips() {
    let request = PtyInRequest {
        input_seq: 42,
        bytes: b"ls -l\r".to_vec(),
    };
    let encoded = request.encode();
    assert_eq!(&encoded[..8], &42u64.to_be_bytes());
    assert_eq!(PtyInRequest::decode(&encoded, "input").unwrap(), request);
}

/// An input payload above the maximum is refused, because the bound exists to
/// stop one keystroke storm making the keeper allocate without limit.
#[test]
fn oversized_input_is_refused_rather_than_truncated() {
    let request = PtyInRequest {
        input_seq: 1,
        bytes: vec![0u8; KEEPER_MAX_INPUT_BYTES as usize + 1],
    };
    let encoded = request.encode();
    assert!(matches!(
        PtyInRequest::decode(&encoded, "input"),
        Err(CodecError::InputTooLarge { .. })
    ));
}

/// The three input results are distinguishable by their tag AND their
/// encoding. An Ack carries a reason of zero, which is not a valid reject
/// reason, so a decoder cannot read one as the other.
#[test]
fn the_three_input_results_stay_distinguishable() {
    let ack = PtyInResult::Ack {
        input_seq: 7,
        written: 3,
    };
    let reject = PtyInResult::Reject {
        input_seq: 7,
        reason: PtyInRejectReason::QueueFull,
    };
    let ambiguous = PtyInResult::Ambiguous {
        input_seq: 7,
        written: 2,
        reason: PtyInRejectReason::PartialWrite,
    };

    assert_eq!(
        PtyInResult::decode(MuxFrameType::PtyInAck, &ack.encode()),
        Some(ack.clone())
    );
    assert_eq!(
        PtyInResult::decode(MuxFrameType::PtyInReject, &reject.encode()),
        Some(reject.clone())
    );
    assert_eq!(
        PtyInResult::decode(MuxFrameType::PtyInAmbiguous, &ambiguous.encode()),
        Some(ambiguous.clone())
    );

    // The reason byte is what separates the two failure modes, and a partial
    // write is the one where a retry would duplicate a character.
    assert_eq!(reject.encode()[12], PtyInRejectReason::QueueFull.code());
    assert_eq!(
        ambiguous.encode()[12],
        PtyInRejectReason::PartialWrite.code()
    );
    assert_ne!(
        PtyInResult::Reject {
            input_seq: 7,
            reason: PtyInRejectReason::NoReader
        }
        .encode()[12],
        0
    );
}

/// `[seq:u64][cols:u32][rows:u32]`.
#[test]
fn sequenced_resize_round_trips() {
    let request = ResizeRequest {
        seq: 9,
        cols: 120,
        rows: 40,
    };
    let encoded = request.encode().unwrap();
    assert_eq!(&encoded[..8], &9u64.to_be_bytes());
    assert_eq!(&encoded[8..12], &120u32.to_be_bytes());
    assert_eq!(&encoded[12..16], &40u32.to_be_bytes());
    assert_eq!(ResizeRequest::decode(&encoded, "resize").unwrap(), request);
}

/// A zero or absurd dimension is rejected, never clamped. A clamp silently
/// produces a PTY whose geometry differs from what the client believes, and
/// the client has no way to discover that.
#[test]
fn a_bad_dimension_is_rejected_rather_than_clamped() {
    let zero = ResizeRequest {
        seq: 1,
        cols: 0,
        rows: 10,
    };
    assert!(matches!(zero.encode(), Err(CodecError::BadDimension(0))));

    // A dimension above the maximum cannot be expressed through the typed API
    // at all, because `cols` is a u16. The decode path is where an oversized
    // value can actually arrive, so that is what is asserted below.

    let mut oversized = vec![0u8; 8];
    oversized.extend_from_slice(&u32::MAX.to_be_bytes());
    oversized.extend_from_slice(&10u32.to_be_bytes());
    let on_disk = ResizeRequest::decode(&oversized, "resize");
    assert!(matches!(on_disk, Err(CodecError::BadDimension(_))));
}

/// The terminal-state answer reuses the resize shape, so a client holding a
/// lost `ResizeAck` can decode the recovery with the decoder it already has.
#[test]
fn terminal_state_decodes_with_the_resize_shape() {
    let state = TerminalState {
        applied_seq: 11,
        cols: 80,
        rows: 24,
    };
    let encoded = state.encode().unwrap();
    assert_eq!(TerminalState::decode(&encoded).unwrap(), state);
    assert_eq!(
        ResizeRequest::decode(&encoded, "state").unwrap(),
        ResizeRequest {
            seq: 11,
            cols: 80,
            rows: 24
        }
    );
}

/// Spawn and its acknowledgement are the frames a client acts on, so their
/// JSON shape is part of the wire and is asserted by decoding real bytes.
#[test]
fn spawn_frames_round_trip_through_json() {
    let request = SpawnRequest {
        channel_id: 12,
        cols: 100,
        rows: 30,
        shell_spec: roost_keeper::frames::ShellSpec::default_login("/bin/bash"),
    };
    let frame = MuxFrame::json(MuxFrameType::Spawn, 12, &request).unwrap();
    assert_eq!(frame.parse_json::<SpawnRequest>(), Some(request));

    let ack = SpawnAck {
        channel_id: 12,
        pid: 4242,
    };
    let frame = MuxFrame::json(MuxFrameType::SpawnAck, 12, &ack).unwrap();
    assert_eq!(frame.parse_json::<SpawnAck>(), Some(ack));
}
