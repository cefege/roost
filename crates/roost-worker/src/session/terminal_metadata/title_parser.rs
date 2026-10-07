//! OSC 0/2 title extraction and normalization for one ordered PTY stream.
//!
//! `session::terminal_metadata` owns publication and replay; this parser keeps
//! only bounded partial input between chunks.

use super::{TERMINAL_TITLE_CARRY_CAP, TERMINAL_TITLE_MAX_LENGTH};

/// A normalized title and the key two spinner frames of one title share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleObservation {
    pub title: String,
    pub dedup_key: String,
}

/// v2 `normalizeTerminalTitle`: strip C0/DEL, cap the length, fold braille spinners.
pub fn normalize_terminal_title(raw: &str) -> TitleObservation {
    let mut title = String::new();
    let mut units = 0usize;
    for character in raw
        .chars()
        .filter(|c| !matches!(*c as u32, 0x00..=0x1f | 0x7f))
    {
        units += character.len_utf16();
        if units > TERMINAL_TITLE_MAX_LENGTH {
            break;
        }
        title.push(character);
    }
    let dedup_key = title
        .chars()
        .map(|c| {
            if ('\u{2800}'..='\u{28FF}').contains(&c) {
                '\u{2800}'
            } else {
                c
            }
        })
        .collect();
    TitleObservation { title, dedup_key }
}

/// v2 `TerminalTitleParser`: OSC 0/2 titles from one ordered PTY byte stream.
#[derive(Debug, Default, Clone)]
pub struct TerminalTitleParser {
    carry: String,
    /// An incomplete UTF-8 sequence at the end of the last chunk.
    pending_utf8: Vec<u8>,
}

impl TerminalTitleParser {
    /// The latest complete title in this chunk, if any.
    pub fn push(&mut self, bytes: &[u8]) -> Option<TitleObservation> {
        if bytes.is_empty() {
            return None;
        }
        let mut raw = std::mem::take(&mut self.pending_utf8);
        raw.extend_from_slice(bytes);
        let complete = match std::str::from_utf8(&raw) {
            Ok(_) => raw.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => raw.len(),
        };
        self.pending_utf8 = raw[complete..].to_vec();
        let mut combined = std::mem::take(&mut self.carry);
        combined.push_str(&String::from_utf8_lossy(&raw[..complete]));
        if !combined.contains("\u{1b}]") {
            self.carry = if combined.ends_with('\u{1b}') {
                "\u{1b}".to_owned()
            } else {
                String::new()
            };
            return None;
        }
        let (latest, last_end) = last_osc_title(&combined);
        self.carry = bounded_carry(&combined[last_end..]);
        latest.map(|title| normalize_terminal_title(&title))
    }
}

/// `/\x1b\][02];([^\x07\x1b]*)(?:\x07|\x1b\\)/g`: the last match's body, and
/// the byte offset just past it (0 without a match).
fn last_osc_title(text: &str) -> (Option<String>, usize) {
    let bytes = text.as_bytes();
    let (mut latest, mut last_end, mut start) = (None, 0, 0);
    while start + 3 < bytes.len() {
        let Some(offset) = text[start..].find("\u{1b}]") else {
            break;
        };
        let at = start + offset;
        let body = at + 4;
        if matches!(bytes.get(at + 2), Some(b'0' | b'2')) && bytes.get(at + 3) == Some(&b';') {
            let end = bytes[body.min(bytes.len())..]
                .iter()
                .position(|byte| *byte == 0x07 || *byte == 0x1b)
                .map(|index| body + index);
            let terminated = end.and_then(|end| match bytes[end] {
                0x07 => Some(end + 1),
                _ if bytes.get(end + 1) == Some(&b'\\') => Some(end + 2),
                _ => None,
            });
            if let (Some(end), Some(after)) = (end, terminated) {
                latest = Some(text[body..end].to_owned());
                last_end = after;
                start = after;
                continue;
            }
        }
        start = at + 1;
    }
    (latest, last_end)
}

fn bounded_carry(value: &str) -> String {
    let count = value.chars().count();
    if count <= TERMINAL_TITLE_CARRY_CAP {
        return value.to_owned();
    }
    value
        .chars()
        .skip(count - TERMINAL_TITLE_CARRY_CAP)
        .collect()
}
