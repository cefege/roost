//! The reply lane's core half against a real core: replies leave in the order
//! the probes appeared, a probe split across chunks is answered exactly once,
//! the native set is v2's (a cursor report, nothing else), and a replay never
//! answers history. Ports `apps/worker/tests/terminal/terminal-query-reply-kitty.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_term::{AlacrittyCore, TerminalCore};
use roost_worker::session::query_reply::{
    PRIMARY_DA_REPLY, QUERY_CARRY_MAX, QueryReply, XTVERSION_REPLY, answer_queries,
};

fn answer(core: &mut AlacrittyCore, carry: &mut Vec<u8>, chunk: &[u8]) -> QueryReply {
    answer_queries(carry, Some(core as &mut dyn TerminalCore), chunk)
}

fn answer_fresh(chunk: &[u8]) -> QueryReply {
    answer(&mut AlacrittyCore::new(80, 24), &mut Vec::new(), chunk)
}

/// The Kitty keyboard query is a promise the browser's legacy-key encoder
/// cannot keep. v2's core answered it and the lane muted the answer; this core
/// must not answer it at all, and the lane must stay silent either way.
#[test]
fn a_kitty_keyboard_query_is_never_answered() {
    let mut core = AlacrittyCore::new(80, 24);
    core.write_raw(b"\x1b[?u");
    assert_eq!(core.get_response(), None, "the core makes no Kitty promise");
    assert_eq!(answer_fresh(b"\x1b[?u").bytes, "");
}

#[test]
fn a_cursor_report_still_reaches_the_pty_when_a_kitty_probe_precedes_it() {
    let reply = answer_fresh(b"\x1b[?u\x1b[6n");
    assert_eq!(reply.bytes, "\x1b[1;1R");
    assert_eq!(reply.synth_bytes, 0);
}

#[test]
fn pushing_kitty_flags_never_makes_the_lane_speak() {
    assert_eq!(answer_fresh(b"\x1b[>1u\x1b[?u\x1b[<u").bytes, "");
}

#[test]
fn a_primary_da_sharing_the_chunk_is_answered_in_probe_order() {
    let reply = answer_fresh(b"\x1b[?u\x1b[c\x1b[6n");
    assert_eq!(reply.bytes, format!("{PRIMARY_DA_REPLY}\x1b[1;1R"));
}

/// Concatenating every native ahead of every synthesized reply would answer
/// these backwards; the cursor report also proves the core saw the bytes
/// between the probes before it answered.
#[test]
fn natives_and_synthesized_replies_leave_in_probe_order() {
    let native_first = answer_fresh(b"\x1b[6nab\x1b[c");
    assert_eq!(native_first.bytes, format!("\x1b[1;1R{PRIMARY_DA_REPLY}"));
    let synth_first = answer_fresh(b"ab\x1b[c\x1b[6n");
    assert_eq!(synth_first.bytes, format!("{PRIMARY_DA_REPLY}\x1b[1;3R"));
    assert_eq!(synth_first.native_bytes, "\x1b[1;3R".len());
    assert_eq!(synth_first.synth_bytes, PRIMARY_DA_REPLY.len());
}

/// A probe cut at ANY offset by a chunk boundary is recognised exactly once,
/// and its reply keeps its place ahead of a probe in the next chunk.
#[test]
fn a_probe_split_at_every_offset_is_answered_exactly_once() {
    for (probe, owed) in [
        ("\x1b[c", PRIMARY_DA_REPLY),
        ("\x1b[0c", PRIMARY_DA_REPLY),
        ("\x1b[>q", XTVERSION_REPLY),
        ("\x1b[>0q", XTVERSION_REPLY),
    ] {
        for cut in 1..probe.len() {
            let mut core = AlacrittyCore::new(80, 24);
            let mut carry = Vec::new();
            let first = format!("x{}", &probe[..cut]);
            let second = format!("{}\x1b[6n", &probe[cut..]);
            let mut written = answer(&mut core, &mut carry, first.as_bytes()).bytes;
            written.push_str(&answer(&mut core, &mut carry, second.as_bytes()).bytes);
            assert_eq!(
                written,
                format!("{owed}\x1b[1;2R"),
                "{probe:?} cut after {cut} bytes"
            );
            assert!(carry.is_empty(), "nothing is left pending after the probe");
        }
    }
}

#[test]
fn the_carry_holds_only_an_unterminated_csi() {
    let mut core = AlacrittyCore::new(80, 24);
    let mut carry = Vec::new();
    answer(&mut core, &mut carry, b"plain text");
    assert!(carry.is_empty());
    answer(&mut core, &mut carry, b"tail\x1b");
    assert_eq!(carry, b"\x1b");
    answer(&mut core, &mut carry, b"[?20");
    assert_eq!(carry, b"\x1b[?20");
    answer(&mut core, &mut carry, b"26$p done");
    assert!(
        carry.is_empty(),
        "a completed sequence leaves nothing behind"
    );
}

/// A CSI that never terminates cannot pin worker memory; the abandoned
/// partial is reported and its eventual final byte answers nothing.
#[test]
fn an_unterminated_csi_past_the_cap_is_abandoned() {
    let mut core = AlacrittyCore::new(80, 24);
    let mut carry = Vec::new();
    let runaway = format!("\x1b[{}", "1;".repeat(QUERY_CARRY_MAX));
    let reply = answer(&mut core, &mut carry, runaway.as_bytes());
    assert_eq!(reply.dropped_carry, runaway.len());
    assert!(carry.is_empty());
    assert_eq!(answer(&mut core, &mut carry, b"c").bytes, "");
}

/// A frozen core parses nothing, but the stream moved: the carry advances and
/// nothing is answered until the post-boundary replay.
#[test]
fn the_capture_lane_advances_the_carry_without_touching_the_core() {
    let mut carry = Vec::new();
    let reply = answer_queries(&mut carry, None, b"\x1b[6n\x1b[c\x1b[");
    assert_eq!(reply, QueryReply::default());
    assert_eq!(carry, b"\x1b[");
}

/// A replay is history: whatever it provokes, and anything still queued, is
/// discarded, so the next live answer is not preceded by a stale one.
#[test]
fn a_plain_write_answers_nothing_and_clears_the_queue() {
    let mut core = AlacrittyCore::new(80, 24);
    core.write(b"\x1b[6n\x1b[c\x1b[5n");
    assert_eq!(core.get_response(), None);
    core.write_raw(b"\x1b[6n");
    core.write(b"replayed");
    assert_eq!(
        core.get_response(),
        None,
        "the undrained report was discarded"
    );
}

/// `vte` would otherwise buffer everything after `CSI ? 2026 h` until the
/// block closes, answering a probe inside it only later — out of stream order.
#[test]
fn a_probe_inside_a_synchronized_update_is_answered_at_once() {
    let reply = answer_fresh(b"\x1b[?2026h\x1b[6n");
    assert_eq!(reply.bytes, "\x1b[1;1R");
}

#[test]
fn xtversion_is_answered_only_for_a_zero_parameter() {
    assert_eq!(answer_fresh(b"\x1b[>q").bytes, XTVERSION_REPLY);
    assert_eq!(answer_fresh(b"\x1b[>0q").bytes, XTVERSION_REPLY);
    assert_eq!(answer_fresh(b"\x1b[>1q").bytes, "");
}

/// Probes v2's core left unanswered: the application must see exactly what it
/// saw under v2. Each is withheld, and counted.
fn assert_withheld(probe: &[u8], expected: &str) {
    let reply = answer_fresh(probe);
    assert_eq!(reply.bytes, expected, "{probe:?}");
    assert_eq!(
        reply.withheld_native, 1,
        "{probe:?} was answered by the core"
    );
}

#[test]
fn primary_da_is_answered_once_with_v2s_reply_not_the_cores() {
    assert_withheld(b"\x1b[c", PRIMARY_DA_REPLY);
}

#[test]
fn secondary_da_is_withheld() {
    assert_withheld(b"\x1b[>c", "");
}

#[test]
fn a_device_status_report_is_withheld() {
    assert_withheld(b"\x1b[5n", "");
}

#[test]
fn a_private_mode_report_is_withheld() {
    assert_withheld(b"\x1b[?25$p", "");
}

#[test]
fn an_ansi_mode_report_is_withheld() {
    assert_withheld(b"\x1b[4$p", "");
}

#[test]
fn a_text_area_size_report_is_withheld() {
    assert_withheld(b"\x1b[18t", "");
}
