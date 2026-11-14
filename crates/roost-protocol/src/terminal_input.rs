//! PTY input shaping: newline normalization and the multiline-paste threshold.
//!
//! Shared by browser, coordinator and worker so they agree on PTY bytes.

/// The introducer the terminal reads as "the next bytes are one paste".
pub const BRACKETED_PASTE_START: &str = "\x1b[200~";
/// The terminator that ends a bracketed paste.
pub const BRACKETED_PASTE_END: &str = "\x1b[201~";

const ESCAPE: char = '\x1b';
const CARRIAGE_RETURN: char = '\r';
const LINE_FEED: char = '\n';

/// The carriage return a PTY consumes as Enter, as the bytes a worker writes.
pub const CR_BYTES: [u8; 1] = *b"\r";

/// Two or more unframed line breaks can execute a partial script as it arrives.
pub const MULTILINE_PASTE_MIN_NEWLINES: usize = 2;

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

#[cfg(test)]
mod tests {
    use super::{
        CR_BYTES, MULTILINE_PASTE_MIN_NEWLINES, build_pty_payload, count_line_breaks,
        normalize_terminal_newlines,
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
}
