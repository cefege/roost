//! AWS event-stream (`application/vnd.amazon.eventstream`) binary frame
//! decoding, reversed from the pinned `@smithy/core` eventstream codec
//! (`external/pi/node_modules/@smithy/core/dist-es/submodules/event-streams/`,
//! pi 0.82.1 @ 2efa728): `splitMessage.ts`, `HeaderMarshaller.ts`,
//! `getChunkedStream.ts`. Design §14: self-implemented event-stream decoding,
//! no aws-sdk crates.
//!
//! Frame layout (big-endian):
//! ```text
//! [ total_len u32 ][ headers_len u32 ][ prelude_crc32 u32 ]
//! [ headers ... ][ payload ... ][ message_crc32 u32 ]
//! ```
//! `prelude_crc32` covers the first 8 bytes; `message_crc32` covers
//! everything from byte 8 to `total_len - 4`. CRC is IEEE CRC-32.
//!
//! Intentional differences: none in the frame format itself; the decoder is
//! fed by an incremental byte stream like `getChunkedStream` (frames may span
//! HTTP chunks) and reports the same error messages.

/// `splitMessage` constants.
const PRELUDE_LENGTH: usize = 8;
const CHECKSUM_LENGTH: usize = 4;
const MINIMUM_MESSAGE_LENGTH: usize = PRELUDE_LENGTH + CHECKSUM_LENGTH * 2;

/// CRC-32 (IEEE, polynomial 0xEDB88320) as used by `@aws-crypto/crc32`.
const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

fn crc32(bytes: &[u8]) -> u32 {
    let mut c = !0u32;
    for byte in bytes {
        c = CRC32_TABLE[((c ^ *byte as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

/// Event-stream header value (`HEADER_VALUE_TYPE` tags 0-9).
#[derive(Debug, Clone, PartialEq)]
pub enum HeaderValue {
    Boolean(bool),
    Byte(i8),
    Short(i16),
    Integer(i32),
    Long(i64),
    Binary(Vec<u8>),
    String(String),
    Timestamp(i64),
    Uuid([u8; 16]),
}

impl HeaderValue {
    /// The string value for string-typed headers (`:message-type` and friends).
    pub fn as_str(&self) -> Option<&str> {
        match self {
            HeaderValue::String(value) => Some(value),
            _ => None,
        }
    }
}

/// One decoded event-stream message.
#[derive(Debug, Clone, PartialEq)]
pub struct EventStreamMessage {
    pub headers: Vec<(String, HeaderValue)>,
    pub body: Vec<u8>,
}

impl EventStreamMessage {
    pub fn header(&self, name: &str) -> Option<&HeaderValue> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }

    pub fn header_str(&self, name: &str) -> Option<&str> {
        self.header(name).and_then(HeaderValue::as_str)
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

/// `HeaderMarshaller.parse`.
fn parse_headers(bytes: &[u8]) -> Result<Vec<(String, HeaderValue)>, String> {
    let mut out = Vec::new();
    let mut position = 0usize;
    while position < bytes.len() {
        let name_length = *bytes
            .get(position)
            .ok_or("Truncated event message header name")? as usize;
        position += 1;
        let name_bytes = bytes
            .get(position..position + name_length)
            .ok_or("Truncated event message header name")?;
        let name = String::from_utf8_lossy(name_bytes).into_owned();
        position += name_length;
        let tag = *bytes
            .get(position)
            .ok_or("Truncated event message header value")?;
        position += 1;
        let value = match tag {
            0 => HeaderValue::Boolean(true),
            1 => HeaderValue::Boolean(false),
            2 => {
                let value = *bytes
                    .get(position)
                    .ok_or("Truncated event message header value")?
                    as i8;
                position += 1;
                HeaderValue::Byte(value)
            }
            3 => {
                let slice = bytes
                    .get(position..position + 2)
                    .ok_or("Truncated event message header value")?;
                position += 2;
                HeaderValue::Short(i16::from_be_bytes([slice[0], slice[1]]))
            }
            4 => {
                let slice = bytes
                    .get(position..position + 4)
                    .ok_or("Truncated event message header value")?;
                position += 4;
                HeaderValue::Integer(i32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
            }
            5 => {
                let slice = bytes
                    .get(position..position + 8)
                    .ok_or("Truncated event message header value")?;
                position += 8;
                let mut raw = [0u8; 8];
                raw.copy_from_slice(slice);
                HeaderValue::Long(i64::from_be_bytes(raw))
            }
            6 | 7 => {
                let length_bytes = bytes
                    .get(position..position + 2)
                    .ok_or("Truncated event message header value")?;
                let length = u16::from_be_bytes([length_bytes[0], length_bytes[1]]) as usize;
                position += 2;
                let slice = bytes
                    .get(position..position + length)
                    .ok_or("Truncated event message header value")?;
                position += length;
                if tag == 6 {
                    HeaderValue::Binary(slice.to_vec())
                } else {
                    HeaderValue::String(String::from_utf8_lossy(slice).into_owned())
                }
            }
            8 => {
                let slice = bytes
                    .get(position..position + 8)
                    .ok_or("Truncated event message header value")?;
                position += 8;
                let mut raw = [0u8; 8];
                raw.copy_from_slice(slice);
                HeaderValue::Timestamp(i64::from_be_bytes(raw))
            }
            9 => {
                let slice = bytes
                    .get(position..position + 16)
                    .ok_or("Truncated event message header value")?;
                position += 16;
                let mut raw = [0u8; 16];
                raw.copy_from_slice(slice);
                HeaderValue::Uuid(raw)
            }
            _ => return Err("Unrecognized header type tag".to_owned()),
        };
        out.push((name, value));
    }
    Ok(out)
}

/// `splitMessage`: validates lengths and both CRC32 checksums, then parses
/// the headers. `frame` must be exactly one message.
pub fn split_message(frame: &[u8]) -> Result<EventStreamMessage, String> {
    if frame.len() < MINIMUM_MESSAGE_LENGTH {
        return Err(
            "Provided message too short to accommodate event stream message overhead".to_owned(),
        );
    }
    let message_length = read_u32(frame, 0) as usize;
    if frame.len() != message_length {
        return Err("Reported message length does not match received message length".to_owned());
    }
    let header_length = read_u32(frame, 4) as usize;
    let expected_prelude_checksum = read_u32(frame, PRELUDE_LENGTH);
    let expected_message_checksum = read_u32(frame, frame.len() - CHECKSUM_LENGTH);

    let prelude_checksum = crc32(&frame[..PRELUDE_LENGTH]);
    if expected_prelude_checksum != prelude_checksum {
        return Err(format!(
            "The prelude checksum specified in the message ({expected_prelude_checksum}) does not match the calculated CRC32 checksum ({prelude_checksum})"
        ));
    }
    let message_checksum = crc32(&frame[PRELUDE_LENGTH..frame.len() - CHECKSUM_LENGTH]);
    if message_checksum != expected_message_checksum {
        return Err(format!(
            "The message checksum ({message_checksum}) did not match the expected value of {expected_message_checksum}"
        ));
    }

    let headers_end = PRELUDE_LENGTH + CHECKSUM_LENGTH + header_length;
    if headers_end > frame.len() - CHECKSUM_LENGTH {
        return Err("Reported message length does not match received message length".to_owned());
    }
    let headers = parse_headers(&frame[PRELUDE_LENGTH + CHECKSUM_LENGTH..headers_end])?;
    let body = frame[headers_end..frame.len() - CHECKSUM_LENGTH].to_vec();
    Ok(EventStreamMessage { headers, body })
}

/// Incremental decoder fed by HTTP byte chunks (`getChunkedStream`): frames
/// may be split across chunks and one chunk may carry several frames.
#[derive(Debug, Default)]
pub struct EventStreamDecoder {
    buffer: Vec<u8>,
}

/// P2-4: hard cap on the wire-declared frame length. `total_len` is a
/// u32 read straight off the stream — a corrupted or hostile value (up to
/// 4 GiB) would previously balloon the buffer while the decoder waited for
/// the "complete" frame. 16 MiB is orders of magnitude above any real
/// Bedrock Converse Stream frame (typical frames are < 128 KiB).
pub const MAX_EVENT_STREAM_FRAME_BYTES: usize = 16 * 1024 * 1024;

impl EventStreamDecoder {
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Pops the next complete frame, if one is buffered.
    pub fn next_message(&mut self) -> Option<Result<EventStreamMessage, String>> {
        if self.buffer.len() < 4 {
            return None;
        }
        let message_length = read_u32(&self.buffer, 0) as usize;
        // P2-4: reject a wire-declared length above the frame cap before
        // buffering towards it (the value is attacker/corruption
        // controlled — see [`MAX_EVENT_STREAM_FRAME_BYTES`]).
        if message_length > MAX_EVENT_STREAM_FRAME_BYTES {
            self.buffer.clear();
            return Some(Err(format!(
                "Event stream frame length {message_length} exceeds the \
                 {MAX_EVENT_STREAM_FRAME_BYTES}-byte cap — malformed stream"
            )));
        }
        if self.buffer.len() < message_length {
            return None;
        }
        let frame: Vec<u8> = self.buffer.drain(..message_length).collect();
        Some(split_message(&frame))
    }

    /// End-of-stream check: a partial frame is an error
    /// (`"Truncated event message received."` from `getChunkedStream`).
    pub fn finish(&mut self) -> Result<(), String> {
        if self.buffer.is_empty() {
            Ok(())
        } else {
            Err("Truncated event message received.".to_owned())
        }
    }
}

/// `EventStreamCodec.encode`: builds one frame (used by contract tests to
/// script mock Bedrock responses; only string headers are needed on the wire
/// but all value types are encoded per `HeaderMarshaller.format`).
pub fn encode_message(headers: &[(String, HeaderValue)], body: &[u8]) -> Vec<u8> {
    let mut header_bytes = Vec::new();
    for (name, value) in headers {
        header_bytes.push(name.len() as u8);
        header_bytes.extend_from_slice(name.as_bytes());
        match value {
            HeaderValue::Boolean(flag) => header_bytes.push(if *flag { 0 } else { 1 }),
            HeaderValue::Byte(value) => header_bytes.extend_from_slice(&[2, *value as u8]),
            HeaderValue::Short(value) => {
                header_bytes.push(3);
                header_bytes.extend_from_slice(&value.to_be_bytes());
            }
            HeaderValue::Integer(value) => {
                header_bytes.push(4);
                header_bytes.extend_from_slice(&value.to_be_bytes());
            }
            HeaderValue::Long(value) => {
                header_bytes.push(5);
                header_bytes.extend_from_slice(&value.to_be_bytes());
            }
            HeaderValue::Binary(bytes) => {
                header_bytes.push(6);
                header_bytes.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
                header_bytes.extend_from_slice(bytes);
            }
            HeaderValue::String(text) => {
                header_bytes.push(7);
                header_bytes.extend_from_slice(&(text.len() as u16).to_be_bytes());
                header_bytes.extend_from_slice(text.as_bytes());
            }
            HeaderValue::Timestamp(value) => {
                header_bytes.push(8);
                header_bytes.extend_from_slice(&value.to_be_bytes());
            }
            HeaderValue::Uuid(bytes) => {
                header_bytes.push(9);
                header_bytes.extend_from_slice(bytes);
            }
        }
    }

    let total_length =
        PRELUDE_LENGTH + CHECKSUM_LENGTH + header_bytes.len() + body.len() + CHECKSUM_LENGTH;
    let mut out = Vec::with_capacity(total_length);
    out.extend_from_slice(&(total_length as u32).to_be_bytes());
    out.extend_from_slice(&(header_bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(&crc32(&out).to_be_bytes());
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(body);
    out.extend_from_slice(&crc32(&out[PRELUDE_LENGTH..]).to_be_bytes());
    out
}

/// Convenience: the headers Bedrock ConverseStream frames carry.
pub fn event_frame(event_type: &str, payload: &str) -> Vec<u8> {
    encode_message(
        &[
            (
                ":message-type".to_owned(),
                HeaderValue::String("event".to_owned()),
            ),
            (
                ":event-type".to_owned(),
                HeaderValue::String(event_type.to_owned()),
            ),
            (
                ":content-type".to_owned(),
                HeaderValue::String("application/json".to_owned()),
            ),
        ],
        payload.as_bytes(),
    )
}

/// Convenience: a `:message-type: exception` frame.
pub fn exception_frame(exception_type: &str, payload: &str) -> Vec<u8> {
    encode_message(
        &[
            (
                ":message-type".to_owned(),
                HeaderValue::String("exception".to_owned()),
            ),
            (
                ":exception-type".to_owned(),
                HeaderValue::String(exception_type.to_owned()),
            ),
            (
                ":content-type".to_owned(),
                HeaderValue::String("application/json".to_owned()),
            ),
        ],
        payload.as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc32_known_values() {
        // IEEE CRC-32 check value ("123456789" -> 0xCBF43926).
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn test_roundtrip_event_frame() {
        let frame = event_frame("messageStart", r#"{"role":"assistant"}"#);
        let message = split_message(&frame).expect("decode");
        assert_eq!(message.header_str(":message-type"), Some("event"));
        assert_eq!(message.header_str(":event-type"), Some("messageStart"));
        assert_eq!(message.body, br#"{"role":"assistant"}"#);
    }

    #[test]
    fn test_decoder_incremental_feed() {
        let mut frames = event_frame("messageStart", r#"{"role":"assistant"}"#);
        frames.extend_from_slice(&event_frame("messageStop", r#"{"stopReason":"end_turn"}"#));
        let mut decoder = EventStreamDecoder::new();
        // Feed byte-by-byte: frames must still reassemble.
        let mut messages = Vec::new();
        for byte in &frames {
            decoder.feed(&[*byte]);
            while let Some(message) = decoder.next_message() {
                messages.push(message.expect("frame"));
            }
        }
        decoder.finish().expect("no truncation");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].header_str(":event-type"), Some("messageStop"));
    }

    #[test]
    fn test_finish_truncated() {
        let frame = event_frame("messageStart", r#"{"role":"assistant"}"#);
        let mut decoder = EventStreamDecoder::new();
        decoder.feed(&frame[..frame.len() - 2]);
        assert_eq!(
            decoder.finish(),
            Err("Truncated event message received.".to_owned())
        );
    }

    #[test]
    fn test_split_message_crc_validation() {
        let mut frame = event_frame("messageStart", r#"{"role":"assistant"}"#);
        // Corrupt one payload byte: the message CRC must reject it.
        let last = frame.len() - 5;
        frame[last] ^= 0xFF;
        let error = split_message(&frame).expect_err("crc error");
        assert!(error.contains("message checksum"), "{error}");
    }
}

#[cfg(test)]
mod frame_cap_tests {
    use super::*;

    /// P2-4: a wire-declared frame length above the cap errors immediately
    /// instead of buffering up to the declared length.
    #[test]
    fn oversized_declared_length_errors_without_buffering() {
        let mut decoder = EventStreamDecoder::new();
        // total_len = u32::MAX (prelude first u32, big-endian), plus noise.
        let mut bytes = vec![0xFF, 0xFF, 0xFF, 0xFF];
        bytes.extend_from_slice(&[0u8; 8]);
        decoder.feed(&bytes);
        match decoder.next_message() {
            Some(Err(message)) => assert!(message.contains("exceeds"), "{message}"),
            other => panic!("expected capped error, got {other:?}"),
        }
        // The buffer was drained — no further growth.
        assert!(decoder.buffer.is_empty());
    }
}
