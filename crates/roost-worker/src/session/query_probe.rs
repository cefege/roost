//! Classifies the CSI probes the worker answers itself, what it answers, and
//! which of the core's own replies reach the application.
//! `session::query_reply` calls [`synthesized_reply`] for every complete CSI it
//! tokenizes, [`program_status_probe`] for every OSC 7501 query, and
//! [`forwarded_native`] for every core reply. Pure byte logic. The terminal
//! core answers the CSI probes too, so neither reaches its unhandled-sequence
//! ring; the lane withholds the core's answer and sends this one.

use std::borrow::Cow;

/// A private marker (`<` `=` `>` `?`) occupies the first parameter position.
const PRIVATE_MIN: u8 = 0x3c;
const PRIVATE_MAX: u8 = 0x3f;

/// Primary DA reply: VT220 (62) with sixel graphics (4) and ANSI colour (22) —
/// the universal "I am a terminal" handshake, and the attribute image tools
/// read before they emit sixel.
pub const PRIMARY_DA_REPLY: &str = "\x1b[?62;4;22c";
/// XTVERSION reply, `DCS > | name ST`: a client that version-gates behaviour
/// sees a name instead of silence. v2's bytes, verbatim.
pub const XTVERSION_REPLY: &str = "\x1bP>|wterm(roost)\x1b\\";
/// Feature-detection reply for OSC 7501: the states and kinds this terminal
/// accepts, in the spec's own spacing.
pub const PROGRAM_STATUS_REPLY: &str = concat!(
    "\x1b]7501 ; ? : states=idle,working,done,blocked,error : kinds=permission,question,auth",
    "\x1b\\",
);

/// A synthesized reply for one complete CSI, or `Some("")` to drain a native
/// reply at that probe boundary; `None` means the CSI is not a handled probe.
pub(super) fn synthesized_reply(body: &[u8], final_byte: u8) -> Option<Cow<'static, str>> {
    let private = match body.first() {
        Some(&first) if (PRIVATE_MIN..=PRIVATE_MAX).contains(&first) => first,
        _ => 0,
    };
    let params = if private == 0 { body } else { &body[1..] };
    match (final_byte, private) {
        (b'c', 0) if zero_params(params) => Some(Cow::Borrowed(PRIMARY_DA_REPLY)),
        (b'q', b'>') if zero_params(params) => Some(Cow::Borrowed(XTVERSION_REPLY)),
        (b'u', b'?') if params.is_empty() => Some(Cow::Borrowed("")),
        _ => None,
    }
}

/// Primary DA and XTVERSION both take Ps=0, defaulting to 0 when omitted; any
/// other parameter makes it a different request (`CSI > c` is DA2, `CSI ? … $ p`
/// is DECRQM), tokenized here so it can never split the stream wrongly, and
/// never answered here.
fn zero_params(params: &[u8]) -> bool {
    params.iter().all(|&byte| byte == b'0' || byte == b';')
}

/// Whether a reply the terminal core produced reaches the application: the
/// cursor report, the kitty keyboard flags, the replies image tools wait on
/// before they draw, and the grapheme-clustering mode report (DECRQM 2027)
/// programs read before they turn clustering on. Everything else the core
/// answers is withheld.
pub(super) fn forwarded_native(reply: &str) -> bool {
    is_cursor_position_report(reply)
        || is_kitty_keyboard_report(reply)
        || is_grapheme_mode_report(reply)
        || reply.starts_with("\x1b_G")
        || is_xtsmgraphics_report(reply)
        || is_pixel_size_report(reply, "\x1b[4;")
        || is_pixel_size_report(reply, "\x1b[6;")
}

fn digits_and_semicolons(body: &str) -> bool {
    !body.is_empty()
        && body
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b';')
}

/// `ESC [ <row> ; <col> R`.
fn is_cursor_position_report(reply: &str) -> bool {
    let Some(body) = reply
        .strip_prefix("\x1b[")
        .and_then(|rest| rest.strip_suffix('R'))
    else {
        return false;
    };
    let Some((row, col)) = body.split_once(';') else {
        return false;
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    digits(row) && digits(col)
}

/// `CSI ? flags u`.
fn is_kitty_keyboard_report(reply: &str) -> bool {
    let Some(flags) = reply
        .strip_prefix("\x1b[?")
        .and_then(|rest| rest.strip_suffix('u'))
    else {
        return false;
    };
    flags.bytes().all(|byte| byte.is_ascii_digit())
}

/// `CSI ? 2027 ; Ps $ y`: the DECRQM answer for grapheme clustering only.
fn is_grapheme_mode_report(reply: &str) -> bool {
    reply
        .strip_prefix("\x1b[?2027;")
        .and_then(|rest| rest.strip_suffix("$y"))
        .is_some_and(|state| !state.is_empty() && state.bytes().all(|byte| byte.is_ascii_digit()))
}

/// `CSI ? Pi ; Ps ; Pv S`.
fn is_xtsmgraphics_report(reply: &str) -> bool {
    reply
        .strip_prefix("\x1b[?")
        .and_then(|rest| rest.strip_suffix('S'))
        .is_some_and(digits_and_semicolons)
}

/// `CSI 4 ; h ; w t` (text area) or `CSI 6 ; h ; w t` (cell), by `prefix`.
fn is_pixel_size_report(reply: &str, prefix: &str) -> bool {
    reply
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix('t'))
        .is_some_and(digits_and_semicolons)
}

/// What the bytes at an `ESC ]` say about the OSC 7501 feature query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProgramStatusProbe {
    /// Not the query, or one that can never become it; the core alone sees it.
    NotQuery,
    /// A prefix of the query that the next chunk may complete.
    Partial,
    /// A whole query, this many bytes long including its terminator.
    Complete(usize),
}

const ESC_BYTE: u8 = 0x1b;
const BEL_BYTE: u8 = 0x07;
/// The query body with spaces removed. The spec ignores whitespace around keys
/// and values, so `OSC 7501 ; ? ST` and `OSC 7501;?ST` both ask the question.
const PROGRAM_STATUS_QUERY_BODY: &[u8] = b"7501;?";

/// Classifies the OSC that begins `run`, whose first two bytes are `ESC ]`.
/// `max_len` bounds how long an unterminated run may be held as a carry, so an
/// OSC that never ends cannot pin worker memory.
pub(super) fn program_status_probe(run: &[u8], max_len: usize) -> ProgramStatusProbe {
    let rest = run.get(2..).unwrap_or_default();
    let terminator = rest
        .iter()
        .position(|&byte| byte == BEL_BYTE || byte == ESC_BYTE);
    let body: Vec<u8> = rest[..terminator.unwrap_or(rest.len())]
        .iter()
        .copied()
        .filter(|&byte| byte != b' ')
        .collect();
    let Some(end) = terminator else {
        let may_complete = run.len() <= max_len && PROGRAM_STATUS_QUERY_BODY.starts_with(&body);
        return if may_complete {
            ProgramStatusProbe::Partial
        } else {
            ProgramStatusProbe::NotQuery
        };
    };
    if body != PROGRAM_STATUS_QUERY_BODY {
        return ProgramStatusProbe::NotQuery;
    }
    match rest.get(end) {
        Some(&BEL_BYTE) => ProgramStatusProbe::Complete(end + 3),
        Some(_) => match rest.get(end + 1) {
            Some(&b'\\') => ProgramStatusProbe::Complete(end + 4),
            Some(_) => ProgramStatusProbe::NotQuery,
            None => ProgramStatusProbe::Partial,
        },
        None => ProgramStatusProbe::NotQuery,
    }
}
