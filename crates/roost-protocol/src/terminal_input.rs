//! PTY input shaping: newline normalization, the multiline-paste threshold, and
//! the agent prompt caps.
//!
//! Owned here so the browser, the coordinator and the worker cannot disagree
//! about what bytes reach a PTY. Depends only on `validate`: every cap in this
//! module is a byte cap, because the wire counts bytes.

use crate::ProtocolResult;
use crate::validate::{integer_in_range, max_utf8_bytes, non_empty};

/// The introducer the terminal reads as "the next bytes are one paste".
pub const BRACKETED_PASTE_START: &str = "\x1b[200~";
/// The terminator that ends a bracketed paste.
pub const BRACKETED_PASTE_END: &str = "\x1b[201~";

const ESCAPE: char = '\x1b';
const CARRIAGE_RETURN: char = '\r';
const LINE_FEED: char = '\n';

/// The carriage return a PTY consumes as Enter, as the bytes a worker writes.
///
/// Counted as one byte when a framed write is budgeted, which is why
/// `AGENT_PROMPT_MAX_WRITE_BYTES` adds `CR_BYTES.len()`.
pub const CR_BYTES: [u8; 1] = *b"\r";

/// Two or more unframed line breaks can execute a partial script as it arrives.
pub const MULTILINE_PASTE_MIN_NEWLINES: usize = 2;

/// The prompt's own text, before any framing.
pub const AGENT_PROMPT_MAX_TEXT_BYTES: usize = 16_384;
/// Framing and the submitted CR can add 13 bytes; normalization cannot grow.
pub const AGENT_PROMPT_MAX_WRITE_BYTES: usize = AGENT_PROMPT_MAX_TEXT_BYTES
    + BRACKETED_PASTE_START.len()
    + BRACKETED_PASTE_END.len()
    + CR_BYTES.len();
/// The rejection reason a caller may echo back to an agent.
pub const AGENT_PROMPT_MAX_REASON_LENGTH: usize = 200;
pub const AGENT_PROMPT_WAIT_TIMEOUT_MIN_MS: i64 = 1;
pub const AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS: i64 = 300_000;

const AGENT_PROMPT_TEXT_FIELD: &str = "text";

/// PTYs consume Enter as CR. This keeps CRLF from becoming two Enters.
///
/// Every line-ending form collapses to exactly one CR, so a caller cannot
/// smuggle a second Enter past a PTY that reads one per keystroke.
pub fn normalize_terminal_newlines(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    for (index, current) in text.char_indices() {
        match current {
            CARRIAGE_RETURN => {
                if text[index + 1..].starts_with(LINE_FEED) {
                    continue;
                }
                normalized.push(CARRIAGE_RETURN);
            }
            LINE_FEED => normalized.push(CARRIAGE_RETURN),
            _ => normalized.push(current),
        }
    }
    normalized
}

/// The exact bytes to hand a PTY write.
///
/// Bracketed paste strips every escape byte before framing: a paste that could
/// close its own bracket would be read as terminal input, not as text.
pub fn build_pty_payload(text: &str, bracketed_paste: bool) -> Vec<u8> {
    let normalized = normalize_terminal_newlines(text);
    if !bracketed_paste {
        return normalized.into_bytes();
    }
    let mut payload = String::with_capacity(normalized.len() + BRACKETED_PASTE_START.len() + 1);
    payload.push_str(BRACKETED_PASTE_START);
    for character in normalized.chars() {
        if character != ESCAPE {
            payload.push(character);
        }
    }
    payload.push_str(BRACKETED_PASTE_END);
    payload.into_bytes()
}

/// The number of logical line breaks, for the multiline-paste threshold.
///
/// Scans with the same CRLF-first alternation as `normalize_terminal_newlines`
/// instead of counting the CRs of a normalized copy: this runs on every
/// keystroke a paste is admitted for, and the copy would be pure overhead.
pub fn count_line_breaks(text: &str) -> usize {
    let mut breaks = 0;
    for (index, current) in text.char_indices() {
        match current {
            CARRIAGE_RETURN => {
                if text[index + 1..].starts_with(LINE_FEED) {
                    continue;
                }
                breaks += 1;
            }
            LINE_FEED => breaks += 1,
            _ => {}
        }
    }
    breaks
}

/// The length the prompt caps are enforced against.
///
/// A Zod `.max(n)` counted UTF-16 code units and silently admitted twice the
/// payload for any non-ASCII prompt, so the cap is the byte count.
pub fn agent_prompt_text_byte_length(text: &str) -> usize {
    text.len()
}

/// The `AgentPromptText` equivalent: a non-empty prompt within the byte cap.
pub fn validate_agent_prompt_text(field: &str, text: &str) -> ProtocolResult<()> {
    non_empty(field, text)?;
    max_utf8_bytes(field, text, AGENT_PROMPT_MAX_TEXT_BYTES)
}

/// Whether a prompt would be admitted, for a caller deciding before it writes.
pub fn is_valid_agent_prompt_text(text: &str) -> bool {
    validate_agent_prompt_text(AGENT_PROMPT_TEXT_FIELD, text).is_ok()
}

/// The `AgentPromptWaitTimeoutMs` equivalent: an integer wait window.
pub fn validate_agent_prompt_wait_timeout_ms(field: &str, value: i64) -> ProtocolResult<()> {
    integer_in_range(
        field,
        value,
        AGENT_PROMPT_WAIT_TIMEOUT_MIN_MS,
        AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        AGENT_PROMPT_MAX_TEXT_BYTES, AGENT_PROMPT_MAX_WRITE_BYTES,
        AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS, AGENT_PROMPT_WAIT_TIMEOUT_MIN_MS, CR_BYTES,
        MULTILINE_PASTE_MIN_NEWLINES, agent_prompt_text_byte_length, build_pty_payload,
        count_line_breaks, is_valid_agent_prompt_text, normalize_terminal_newlines,
        validate_agent_prompt_text, validate_agent_prompt_wait_timeout_ms,
    };

    fn framed(text: &str) -> String {
        String::from_utf8(build_pty_payload(text, true)).expect("PTY payload is UTF-8 text")
    }

    #[test]
    fn every_line_ending_form_becomes_one_carriage_return() {
        assert_eq!(normalize_terminal_newlines("a\r\nb\nc\rd"), "a\rb\rc\rd");
        assert_eq!(normalize_terminal_newlines("\r\r\n"), "\r\r");
        assert_eq!(CR_BYTES, [b'\r']);
    }

    #[test]
    fn bracketed_paste_frames_and_strips_embedded_escape_bytes() {
        assert_eq!(framed("one\ntwo"), "\x1b[200~one\rtwo\x1b[201~");
        // A paste that could close its own bracket would be read as input.
        assert_eq!(
            framed("safe\x1b[201~text"),
            "\x1b[200~safe[201~text\x1b[201~"
        );
        assert_eq!(
            build_pty_payload("\x1braw", false),
            b"\x1braw".to_vec(),
            "escape bytes are only stripped inside a bracketed paste"
        );
    }

    #[test]
    fn line_breaks_are_counted_logically_not_per_byte() {
        assert_eq!(count_line_breaks("one\r\ntwo\nthree\rfour"), 3);
        assert_eq!(count_line_breaks("one line"), 0);
        assert_eq!(count_line_breaks("\r\r\n\n"), 3);
        assert_eq!(MULTILINE_PASTE_MIN_NEWLINES, 2);
    }

    #[test]
    fn the_prompt_cap_is_a_byte_cap_at_exactly_the_limit() {
        let at_limit = "a".repeat(AGENT_PROMPT_MAX_TEXT_BYTES);
        assert!(is_valid_agent_prompt_text(&at_limit));
        assert!(!is_valid_agent_prompt_text(
            &"a".repeat(AGENT_PROMPT_MAX_TEXT_BYTES + 1)
        ));
        assert!(!is_valid_agent_prompt_text(""));
        assert!(is_valid_agent_prompt_text(" "));
    }

    #[test]
    fn a_multibyte_prompt_is_refused_before_its_byte_cap() {
        // 8192 two-byte code points: 8192 UTF-16 units, exactly 16384 bytes.
        let exact_boundary = "é".repeat(AGENT_PROMPT_MAX_TEXT_BYTES / 2);
        assert_eq!(
            exact_boundary.chars().count(),
            AGENT_PROMPT_MAX_TEXT_BYTES / 2
        );
        assert_eq!(
            agent_prompt_text_byte_length(&exact_boundary),
            AGENT_PROMPT_MAX_TEXT_BYTES
        );
        assert!(validate_agent_prompt_text("text", &exact_boundary).is_ok());
        assert!(validate_agent_prompt_text("text", &format!("{exact_boundary}a")).is_err());
    }

    #[test]
    fn the_largest_framed_write_matches_the_write_budget() {
        let largest = build_pty_payload(&"a".repeat(AGENT_PROMPT_MAX_TEXT_BYTES), true).len()
            + CR_BYTES.len();
        assert_eq!(largest, AGENT_PROMPT_MAX_WRITE_BYTES);
    }

    #[test]
    fn the_wait_window_is_an_integer_between_its_endpoints() {
        for accepted in [
            AGENT_PROMPT_WAIT_TIMEOUT_MIN_MS,
            AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS,
        ] {
            assert!(validate_agent_prompt_wait_timeout_ms("wait_timeout_ms", accepted).is_ok());
        }
        for refused in [0, AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS + 1] {
            assert!(validate_agent_prompt_wait_timeout_ms("wait_timeout_ms", refused).is_err());
        }
    }
}
