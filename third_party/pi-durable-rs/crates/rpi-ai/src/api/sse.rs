//! Port of the SSE decoder (`iterateSseMessages`, `decodeSseLine`,
//! `consumeLine`, `flushSseEvent`) from
//! `packages/ai/src/api/anthropic-messages.ts` @ pi 0.82.1 (2efa728).
//!
//! Shared by all SSE-based adapters (anthropic-messages, openai-completions,
//! openai-responses): bytes are decoded incrementally (mirroring
//! `TextDecoder` with `{ stream: true }`, invalid sequences replaced with
//! U+FFFD), lines split on `\n` / `\r` / `\r\n`, `:` lines are comments,
//! multi-line `data:` fields join with `\n`, and a blank line dispatches the
//! event. [`SseDecoder::finish`] processes the unterminated tail and flushes a
//! trailing event, matching the upstream generator's end-of-stream behavior.

/// P2-4: hard cap on a single SSE line (an unterminated line grows the
/// decoder buffer without bound — a misbehaving gateway streaming bytes
/// without a newline would balloon host memory until the idle timeout).
/// 1 MiB is orders of magnitude above any legitimate provider event
/// (typical SSE events are < 64 KiB).
pub const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;

/// A dispatched server-sent event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSentEvent {
    /// The `event:` field, if present.
    pub event: Option<String>,
    /// All `data:` lines joined with `\n`.
    pub data: String,
    /// Raw lines that formed this event (for error reporting).
    pub raw: Vec<String>,
}

#[derive(Default)]
struct DecoderState {
    event: Option<String>,
    data: Vec<String>,
    raw: Vec<String>,
}

fn flush_sse_event(state: &mut DecoderState) -> Option<ServerSentEvent> {
    if state.event.is_none() && state.data.is_empty() {
        return None;
    }
    let event = ServerSentEvent {
        event: state.event.take(),
        data: state.data.join("\n"),
        raw: std::mem::take(&mut state.raw),
    };
    state.data.clear();
    Some(event)
}

fn decode_sse_line(line: &str, state: &mut DecoderState) -> Option<ServerSentEvent> {
    if line.is_empty() {
        return flush_sse_event(state);
    }

    state.raw.push(line.to_owned());
    if line.starts_with(':') {
        return None;
    }

    let delimiter_index = line.find(':');
    let field_name = match delimiter_index {
        Some(index) => &line[..index],
        None => line,
    };
    let mut value = match delimiter_index {
        Some(index) => &line[index + 1..],
        None => "",
    };
    if let Some(stripped) = value.strip_prefix(' ') {
        value = stripped;
    }

    if field_name == "event" {
        state.event = Some(value.to_owned());
    } else if field_name == "data" {
        state.data.push(value.to_owned());
    }

    None
}

fn next_line_break_index(text: &str) -> Option<usize> {
    let carriage_return_index = text.find('\r');
    let newline_index = text.find('\n');
    match (carriage_return_index, newline_index) {
        (Some(cr), Some(nl)) => Some(cr.min(nl)),
        (Some(cr), None) => Some(cr),
        (None, Some(nl)) => Some(nl),
        (None, None) => None,
    }
}

/// Splits the first line off `text`; `None` when no line break is present.
/// `\r` and `\n` are ASCII, so byte indexing never splits a UTF-8 sequence.
fn consume_line(text: &str) -> Option<(&str, &str)> {
    let line_break_index = next_line_break_index(text)?;
    let mut next_index = line_break_index + 1;
    if text.as_bytes()[line_break_index] == b'\r' && text.as_bytes().get(next_index) == Some(&b'\n')
    {
        next_index += 1;
    }
    Some((&text[..line_break_index], &text[next_index..]))
}

/// Incremental SSE decoder over a byte stream.
#[derive(Default)]
pub struct SseDecoder {
    /// Undecoded byte tail (an incomplete UTF-8 sequence).
    utf8_tail: Vec<u8>,
    /// Decoded text not yet consumed as a full line.
    buffer: String,
    state: DecoderState,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes as much of the pending byte buffer as valid UTF-8 allows,
    /// replacing invalid sequences with U+FFFD (`TextDecoder` default). An
    /// incomplete trailing sequence stays buffered unless `flush` is set.
    fn decode_bytes(&mut self, flush: bool) -> String {
        let mut decoded = String::new();
        let mut bytes = std::mem::take(&mut self.utf8_tail);
        let mut offset = 0;
        while offset < bytes.len() {
            match std::str::from_utf8(&bytes[offset..]) {
                Ok(valid) => {
                    decoded.push_str(valid);
                    offset = bytes.len();
                }
                Err(error) => {
                    let valid_up_to = error.valid_up_to();
                    // invariant: from_utf8 guarantees bytes[..valid_up_to] is valid UTF-8
                    decoded.push_str(
                        std::str::from_utf8(&bytes[offset..offset + valid_up_to]).unwrap_or(""),
                    );
                    offset += valid_up_to;
                    match error.error_len() {
                        Some(invalid_len) => {
                            decoded.push('\u{FFFD}');
                            offset += invalid_len;
                        }
                        None => {
                            if flush {
                                decoded.push('\u{FFFD}');
                                offset = bytes.len();
                            } else {
                                // Incomplete trailing sequence: keep for the next chunk.
                                break;
                            }
                        }
                    }
                }
            }
        }
        bytes.drain(..offset);
        self.utf8_tail = bytes;
        decoded
    }

    fn drain_lines(&mut self, events: &mut Vec<ServerSentEvent>) {
        while let Some((line, rest)) = consume_line(&self.buffer) {
            let line = line.to_owned();
            self.buffer = rest.to_owned();
            if let Some(event) = decode_sse_line(&line, &mut self.state) {
                events.push(event);
            }
        }
    }

    /// Feeds a chunk of the response body; returns the events completed by
    /// it. Errors when the unterminated line tail exceeds
    /// [`MAX_SSE_LINE_BYTES`] (P2-4 — a malformed stream must not balloon
    /// host memory until the idle timeout).
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<ServerSentEvent>, String> {
        self.utf8_tail.extend_from_slice(bytes);
        let decoded = self.decode_bytes(false);
        self.buffer.push_str(&decoded);
        let mut events = Vec::new();
        self.drain_lines(&mut events);
        // Bound only the *unterminated* tail (drain first, check after): a
        // single chunk carrying several complete lines may legitimately
        // exceed the cap as a whole — rejecting it up front would fail a
        // well-formed stream on a fast local gateway with a large read
        // buffer.
        self.check_tail_bound()?;
        Ok(events)
    }

    /// End-of-stream flush: decode the byte tail, process the unterminated
    /// line tail, and dispatch a trailing event if one is pending.
    pub fn finish(mut self) -> Result<Vec<ServerSentEvent>, String> {
        let decoded = self.decode_bytes(true);
        self.buffer.push_str(&decoded);
        let mut events = Vec::new();
        self.drain_lines(&mut events);
        self.check_tail_bound()?;
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            if let Some(event) = decode_sse_line(&line, &mut self.state) {
                events.push(event);
            }
        }
        if let Some(event) = flush_sse_event(&mut self.state) {
            events.push(event);
        }
        Ok(events)
    }

    /// P2-4: the unterminated tail must stay bounded — a stream of bytes
    /// with no newline ever arriving would otherwise grow the line buffer
    /// without limit. Checked *after* [`SseDecoder::drain_lines`] so only
    /// the tail (not a chunk of complete lines) is bounded.
    fn check_tail_bound(&self) -> Result<(), String> {
        if self.buffer.len() > MAX_SSE_LINE_BYTES {
            return Err(format!(
                "SSE line exceeds {MAX_SSE_LINE_BYTES} bytes without a newline — \
                 malformed stream"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(chunks: &[&[u8]]) -> Vec<ServerSentEvent> {
        let mut decoder = SseDecoder::new();
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(decoder.feed(chunk).unwrap());
        }
        events.extend(decoder.finish().unwrap());
        events
    }

    #[test]
    fn test_basic_event_dispatch() {
        let events = feed_all(&[b"event: message_start\ndata: {\"a\":1}\n\n"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("message_start"));
        assert_eq!(events[0].data, "{\"a\":1}");
        assert_eq!(
            events[0].raw,
            vec!["event: message_start", "data: {\"a\":1}"]
        );
    }

    #[test]
    fn test_multiline_data_joined_with_newline() {
        let events = feed_all(&[b"data: first\ndata: second\n\n"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, None);
        assert_eq!(events[0].data, "first\nsecond");
    }

    #[test]
    fn test_comment_and_crlf_and_lone_cr() {
        // Comment lines are ignored; CRLF and lone CR both terminate lines.
        let events = feed_all(&[b": comment\r\ndata: a\r\n\r\ndata: b\r\r"]);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "a");
        assert_eq!(events[1].data, "b");
        // The comment is part of the first event's raw lines upstream.
        assert_eq!(events[0].raw, vec![": comment", "data: a"]);
    }

    #[test]
    fn test_split_across_chunks() {
        let events = feed_all(&[b"data: hel", b"lo wor", b"ld\n\ndata: 2\n\n"]);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "hello world");
        assert_eq!(events[1].data, "2");
    }

    #[test]
    fn test_utf8_split_across_chunks() {
        // "🙈" is 4 bytes; split inside the sequence.
        let full = "data: 🙈\n\n";
        let bytes = full.as_bytes();
        let split = bytes.iter().position(|_| false); // unused; manual split below
        let _ = split;
        let cut = "data: ".len() + 2;
        let events = feed_all(&[&bytes[..cut], &bytes[cut..]]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "🙈");
    }

    #[test]
    fn test_trailing_event_without_blank_line() {
        // Stream ends without the dispatching blank line: finish() flushes.
        let events = feed_all(&[b"event: message_stop\ndata: {}"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("message_stop"));
    }

    #[test]
    fn test_data_without_event_and_space_handling() {
        // `data:` with no space and with a space both decode; a second leading
        // space is significant.
        let events = feed_all(&[b"data:nospace\n\ndata:  two-spaces\n\n"]);
        assert_eq!(events[0].data, "nospace");
        assert_eq!(events[1].data, " two-spaces");
    }

    #[test]
    fn test_invalid_utf8_replaced() {
        let events = feed_all(&[b"data: a\xffb\n\n"]);
        assert_eq!(events[0].data, "a\u{FFFD}b");
    }
}

#[cfg(test)]
mod line_bound_tests {
    use super::*;

    /// P2-4: a stream of bytes with no newline ever arriving must error at
    /// the 1 MiB line cap instead of growing the buffer unboundedly.
    #[test]
    fn unterminated_line_over_cap_errors() {
        let mut decoder = SseDecoder::new();
        // Fill below the cap first — still fine.
        let chunk = "x".repeat(64 * 1024);
        assert!(decoder.feed(chunk.as_bytes()).is_ok());
        // Cross the cap without any newline.
        let big = "y".repeat(MAX_SSE_LINE_BYTES);
        let result = decoder.feed(big.as_bytes());
        assert!(
            result.is_err(),
            "over-cap unterminated line must error, not buffer"
        );
    }

    /// Legitimate large events below the cap still decode normally.
    #[test]
    fn large_line_below_cap_decodes() {
        let mut decoder = SseDecoder::new();
        let payload = "a".repeat(512 * 1024);
        let line = format!("data: {payload}\n\n");
        let events = decoder.feed(line.as_bytes()).expect("below cap");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, payload);
    }

    /// rc.12 review (P2): the cap bounds the *unterminated tail*, not the
    /// chunk — a single read carrying several complete events whose total
    // exceeds 1 MiB is a well-formed stream and must decode.
    #[test]
    fn multi_complete_lines_chunk_over_total_cap_decodes() {
        let mut decoder = SseDecoder::new();
        // Three ~384 KiB events: 1 152 KiB total > 1 MiB, each individually
        // far below the per-line cap and newline-terminated.
        let payload = "b".repeat(384 * 1024);
        let chunk = format!("data: {payload}\n\ndata: {payload}\n\ndata: {payload}\n\n");
        let events = decoder
            .feed(chunk.as_bytes())
            .expect("complete lines decode");
        assert_eq!(events.len(), 3);
        assert!(events.iter().all(|event| event.data == payload));
        // Nothing left buffered: the tail check trivially passes.
        assert!(decoder.finish().expect("finish ok").is_empty());
    }

    /// rc.12 review (P1): the panic trigger pinned at the decoder level — a
    /// tail of exactly `MAX_SSE_LINE_BYTES` bytes (feed passes: `>` strict)
    /// ending in an incomplete UTF-8 sequence; `finish()` flushes U+FFFD
    /// (3 bytes) pushing it over the cap and must return `Err`, never
    /// panic. Adapters propagate this as a terminal fail-fast error.
    #[test]
    fn finish_cap_sized_tail_with_incomplete_utf8_errors() {
        let mut decoder = SseDecoder::new();
        let mut body = Vec::with_capacity(MAX_SSE_LINE_BYTES);
        body.extend_from_slice(b"data: ");
        body.resize(MAX_SSE_LINE_BYTES - 1, b'x');
        body.push(0xF0); // incomplete 4-byte sequence start
        assert_eq!(body.len(), MAX_SSE_LINE_BYTES);
        assert!(decoder.feed(&body).is_ok(), "tail at the cap passes feed");
        let result = decoder.finish();
        assert!(
            result.is_err(),
            "finish must fail after the U+FFFD flush crosses the cap"
        );
    }
}
