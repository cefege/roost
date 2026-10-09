//! Classifies the CSI probes the worker answers itself, what it answers, and
//! which of the core's own replies reach the application.
//! `session::query_reply` calls [`synthesized_reply`] for every complete CSI it
//! tokenizes and [`forwarded_native`] for every core reply. Pure byte logic. The terminal core answers both probes
//! too, so neither reaches its unhandled-sequence ring; the lane withholds the
//! core's answer and sends this one.

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
/// cursor report, the kitty keyboard flags, and the replies image tools wait
/// on before they draw. Everything else the core answers is withheld.
pub(super) fn forwarded_native(reply: &str) -> bool {
    is_cursor_position_report(reply)
        || is_kitty_keyboard_report(reply)
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
