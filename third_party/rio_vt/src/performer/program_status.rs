//! OSC 7501 Program Status Protocol: a program reports whether it is working,
//! blocked, done or failed. ROOST PATCH R8 (see `ROOST-PATCHES.md`): the
//! dispatcher hands the sequence here and gets back a validated report or
//! nothing. Record retention (replace, clear subtree, lifetime, record cap)
//! belongs to Roost and lives in `roost-protocol`.
//!
//! Upstream rio has no OSC length cap, so the spec's whole-sequence cap is
//! enforced here before any field is decoded.

use crate::event::{ProgramStatusKind, ProgramStatusReport, ProgramStatusState};

/// The whole sequence, from `ESC ]` to its terminator, in bytes.
const SEQUENCE_MAX_BYTES: usize = 4096;
/// `ESC ] 7501 ;` is 7 bytes and the terminator at most 2, so a body longer
/// than this cannot fit the cap whichever terminator the program used.
const FRAMING_BYTES: usize = 9;
const KEY_MAX_BYTES: usize = 16;
const ID_MAX_BYTES: usize = 128;
const ID_MAX_LEVELS: usize = 8;
const SEGMENT_MAX_BYTES: usize = 32;
const PROGRESS_MAX: u16 = 100;
const TITLE_ENCODED_MAX_BYTES: usize = 256;
const TITLE_DECODED_MAX_BYTES: usize = 192;
const MSG_ENCODED_MAX_BYTES: usize = 2732;
const MSG_DECODED_MAX_BYTES: usize = 2048;

/// The report `params` (`7501;<pairs>`) carries, or `None` when the spec says
/// to ignore or discard it: over the sequence cap, a missing or unknown state,
/// a malformed id, or a text field that breaks its limit, does not decode, or
/// holds a control character. A malformed pair alone is skipped, not the report.
pub(super) fn parse_program_status(params: &[&[u8]]) -> Option<ProgramStatusReport> {
    let body = params.get(1..)?.join(&b';');
    if body.len() + FRAMING_BYTES > SEQUENCE_MAX_BYTES {
        return None;
    }

    let mut draft = Draft::default();
    for pair in body.split(|byte| *byte == b':') {
        let Some(equals) = pair.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let (key, value) = (pair[..equals].trim_ascii(), pair[equals + 1..].trim_ascii());
        if !is_key(key) || !value.iter().all(|byte| is_value_byte(*byte)) {
            continue;
        }
        match key {
            b"state" => draft.state = Some(parse_state(value)?),
            b"id" => draft.id = Some(parse_id(value)?),
            b"kind" => draft.kind = parse_kind(value),
            b"app" if is_segment(value) => {
                draft.app = Some(String::from_utf8_lossy(value).into_owned());
            }
            b"progress" => {
                if let Some(percent) = parse_progress(value) {
                    draft.progress = Some(percent);
                }
            }
            b"title" => {
                draft.title = Some(decode_text(
                    value,
                    TITLE_ENCODED_MAX_BYTES,
                    TITLE_DECODED_MAX_BYTES,
                )?);
            }
            b"msg" => {
                draft.msg = Some(decode_text(
                    value,
                    MSG_ENCODED_MAX_BYTES,
                    MSG_DECODED_MAX_BYTES,
                )?);
            }
            _ => {}
        }
    }
    draft.finish()
}

#[derive(Default)]
struct Draft {
    state: Option<ProgramStatusState>,
    id: Option<String>,
    kind: Option<ProgramStatusKind>,
    app: Option<String>,
    title: Option<String>,
    msg: Option<String>,
    progress: Option<u8>,
}

impl Draft {
    /// Keeps `kind` only with `blocked` and `progress` only with `working` or
    /// `blocked`, as the spec defines which fields each state carries.
    fn finish(self) -> Option<ProgramStatusReport> {
        let state = self.state?;
        let kind = match state {
            ProgramStatusState::Blocked => self.kind,
            _ => None,
        };
        let progress = match state {
            ProgramStatusState::Working | ProgramStatusState::Blocked => self.progress,
            _ => None,
        };
        Some(ProgramStatusReport {
            id: self.id,
            state,
            kind,
            app: self.app,
            title: self.title,
            msg: self.msg,
            progress,
        })
    }
}

fn parse_state(value: &[u8]) -> Option<ProgramStatusState> {
    Some(match value {
        b"idle" => ProgramStatusState::Idle,
        b"working" => ProgramStatusState::Working,
        b"done" => ProgramStatusState::Done,
        b"blocked" => ProgramStatusState::Blocked,
        b"error" => ProgramStatusState::Error,
        b"clear" => ProgramStatusState::Clear,
        _ => return None,
    })
}

fn parse_kind(value: &[u8]) -> Option<ProgramStatusKind> {
    Some(match value {
        b"permission" => ProgramStatusKind::Permission,
        b"question" => ProgramStatusKind::Question,
        b"auth" => ProgramStatusKind::Auth,
        _ => return None,
    })
}

/// `segment("/"segment)*`, at most `ID_MAX_LEVELS` levels and `ID_MAX_BYTES`
/// bytes. Any violation rejects the whole report, not just the field.
fn parse_id(value: &[u8]) -> Option<String> {
    if value.len() > ID_MAX_BYTES {
        return None;
    }
    let segments: Vec<&[u8]> = value.split(|byte| *byte == b'/').collect();
    if segments.len() > ID_MAX_LEVELS || !segments.iter().all(|segment| is_segment(segment)) {
        return None;
    }
    Some(String::from_utf8_lossy(value).into_owned())
}

fn parse_progress(value: &[u8]) -> Option<u8> {
    let percent = std::str::from_utf8(value).ok()?.parse::<u16>().ok()?;
    if percent > PROGRESS_MAX {
        return None;
    }
    u8::try_from(percent).ok()
}

/// The encoded length is checked before decoding, so an oversized field costs
/// no allocation. `None` discards the whole report: a field that breaks its
/// limit, does not decode, is not UTF-8, or holds a control character.
fn decode_text(value: &[u8], encoded_max: usize, decoded_max: usize) -> Option<String> {
    if value.len() > encoded_max {
        return None;
    }
    let bytes = crate::simd_base64::decode(value)?;
    if bytes.len() > decoded_max {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    if text.chars().any(char::is_control) {
        return None;
    }
    Some(text)
}

fn is_key(key: &[u8]) -> bool {
    !key.is_empty() && key.len() <= KEY_MAX_BYTES && key.iter().all(u8::is_ascii_lowercase)
}

/// The spec's value alphabet; a value outside it makes its pair malformed.
fn is_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b',' | b'+' | b'/' | b'=' | b'-')
}

/// One `[A-Za-z0-9_.+-]{1,32}` segment: the alphabet of ids and of `app`.
fn is_segment(segment: &[u8]) -> bool {
    (1..=SEGMENT_MAX_BYTES).contains(&segment.len())
        && segment
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'+' | b'-'))
}
