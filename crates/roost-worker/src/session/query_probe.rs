//! Classifies the CSI probes the worker answers itself, and what it answers.
//! `session::query_reply` calls [`synthesized_reply`] for every complete CSI it
//! tokenizes; `session::unhandled_seq` calls [`answered_by_worker`] so a probe
//! Roost answers is not reported as unhandled. Pure byte logic, no state.

use std::borrow::Cow;

/// A private marker (`<` `=` `>` `?`) occupies the first parameter position.
const PRIVATE_MIN: u8 = 0x3c;
const PRIVATE_MAX: u8 = 0x3f;

/// Primary DA reply: VT100 with Advanced Video Option — the universal "I am a
/// terminal" handshake, enough to unblock any DA-gated init.
pub const PRIMARY_DA_REPLY: &str = "\x1b[?1;2c";
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
        (b'S', b'?') => xtsmgraphics_reply(params).map(Cow::Owned),
        _ => None,
    }
}

/// Whether a CSI with this final byte and private marker is a probe the worker
/// answers itself, so the terminal core's silence on it is not a gap.
pub fn answered_by_worker(final_byte: u8, private: u8) -> bool {
    matches!((final_byte, private), (b'q', b'>') | (b'S', b'?'))
}

/// Primary DA and XTVERSION both take Ps=0, defaulting to 0 when omitted; any
/// other parameter makes it a different request (`CSI > c` is DA2, `CSI ? … $ p`
/// is DECRQM), tokenized here so it can never split the stream wrongly, and
/// never answered here.
fn zero_params(params: &[u8]) -> bool {
    params.iter().all(|&byte| byte == b'0' || byte == b';')
}

/// XTSMGRAPHICS, `CSI ? Pi ; Pa ; Pv S`: agent TUIs probe it for sixel/ReGIS
/// support and wait on the answer. xterm replies `CSI ? Pi ; Ps ; Pv S` with
/// Ps 0 success, 1 error in Pi, 2 error in Pa, 3 failure; Roost has no
/// graphics, so a well-formed probe is answered as a failure.
fn xtsmgraphics_reply(params: &[u8]) -> Option<String> {
    if !params
        .iter()
        .all(|&byte| byte.is_ascii_digit() || byte == b';')
    {
        return None;
    }
    let mut fields = params.split(|&byte| byte == b';');
    let item = parse_field(fields.next()?)?;
    let action = parse_field(fields.next()?)?;
    let status = if !(1..=3).contains(&item) {
        1
    } else if !(1..=4).contains(&action) {
        2
    } else {
        3
    };
    Some(format!("\x1b[?{item};{status};0S"))
}

/// One numeric parameter; an empty field reads as 0, an overflow as no probe.
fn parse_field(field: &[u8]) -> Option<u32> {
    if field.is_empty() {
        return Some(0);
    }
    std::str::from_utf8(field).ok()?.parse().ok()
}
