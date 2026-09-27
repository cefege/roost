//! The frame-length bounds on the keeper wire codec, split out of
//! `codec_wire.rs` so neither file sits at the cap after `cargo fmt`.

use roost_keeper::codec::{CodecError, FrameDecoder, KEEPER_MAX_MUX_FRAME_BYTES,
    MUX_FRAME_HEADER_BYTES, MuxFrameType, StreamEvent};

fn frame_events(bytes: &[u8]) -> Vec<StreamEvent> {
    FrameDecoder::new().push(bytes)
}

/// A length below the three-byte minimum is the mirror of the oversized case,
/// and it is the one that used to be unguarded.
///
/// The decoder drains `claimed` bytes and then indexes `frame[0]`, `frame[1]`
/// and `frame[2]` for the tag and channel id. With only the upper bound, a
/// peer sending `00 00 00 00` produced an empty frame, and the index panicked.
/// The panic unwinds the serve loop and the keeper's `main`, so four bytes
/// written to the socket by any same-uid process destroyed every PTY on the
/// machine. v2 refused the same frame with a `RangeError` at
/// `protocol-envelope.ts:114`; the port kept the indexing and lost the check.
///
/// Every short length is exercised, not just zero, because 1 and 2 reach the
/// index too — each one is one byte short of the channel id it reads.
#[test]
fn an_undersized_length_is_refused_instead_of_indexing_past_the_frame() {
    for claimed in 0..MUX_FRAME_HEADER_BYTES {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&claimed.to_be_bytes());
        // Enough trailing bytes that the decoder believes the whole frame has
        // arrived. Without them the loop breaks on an incomplete prefix and
        // the short body is never indexed, which is why the panic needed an
        // attacker who pads as well as truncates.
        bytes.extend_from_slice(&[0u8; 16]);

        let events = frame_events(&bytes);
        assert!(
            matches!(events.as_slice(), [StreamEvent::Failed(CodecError::LengthMismatch { claimed: c, .. })] if *c == claimed),
            "a frame claiming {claimed} bytes must be refused, not indexed; got {events:?}"
        );
    }
}

/// The boundary itself must still decode. A lower bound that is one too
/// aggressive refuses every real frame, which fails loudly and is at least
/// visible — but a lower bound that is one too lax is the bug this test
/// exists to catch, and the two are the same line of code.
#[test]
fn the_smallest_legal_frame_still_decodes() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MUX_FRAME_HEADER_BYTES.to_be_bytes());
    bytes.extend_from_slice(&[MuxFrameType::PtyOut.tag(), 0x00, 0x2a]);

    let events = frame_events(&bytes);
    assert!(
        matches!(
            events.as_slice(),
            [StreamEvent::Frame { frame_type: Some(MuxFrameType::PtyOut), channel_id: 42, payload, .. }]
                if payload.is_empty()
        ),
        "a three-byte body is the smallest legal frame and must decode; got {events:?}"
    );
}
