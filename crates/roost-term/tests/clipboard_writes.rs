//! OSC 52 through the core: a live store is handed over decoded and once, a
//! read is never honoured, an oversized store is dropped, and a store parsed
//! from history never fires. The worker forwards whatever this queue holds to
//! the operator's clipboard, so each of these is a clipboard the operator did
//! not expect to change.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_term::{AlacrittyCore, TerminalCore};

/// An OSC 52 store of `count * 3` bytes of `A`: `QUFB` is the base64 of `AAA`.
fn store_of_repeated_a(count: usize) -> String {
    format!("\x1b]52;c;{}\x07", "QUFB".repeat(count))
}

#[test]
fn live_stores_arrive_decoded_in_order_with_either_terminator() {
    let mut core = AlacrittyCore::new(80, 24);
    core.write_raw(b"\x1b]52;c;SGVsbG8=\x07\x1b]52;c;V29ybGQ=\x1b\\");

    assert_eq!(core.take_clipboard_writes(), ["Hello", "World"]);
    assert!(
        core.take_clipboard_writes().is_empty(),
        "a taken write is not handed over twice"
    );
}

#[test]
fn a_clipboard_read_is_never_answered_or_forwarded() {
    let mut core = AlacrittyCore::new(80, 24);
    core.write_raw(b"\x1b]52;c;?\x07");
    assert!(core.take_clipboard_writes().is_empty());
    assert_eq!(
        core.get_response(),
        None,
        "the clipboard is never read back"
    );
}

#[test]
fn a_store_over_the_cap_is_dropped_and_one_at_the_cap_is_kept() {
    let cap = 256 * 1024;
    let mut core = AlacrittyCore::new(80, 24);
    core.write_raw(store_of_repeated_a(cap / 3 + 1).as_bytes());
    assert!(core.take_clipboard_writes().is_empty());

    core.write_raw(store_of_repeated_a(cap / 3).as_bytes());
    let kept = core.take_clipboard_writes();
    assert_eq!(kept.len(), 1);
    assert!(kept[0].len() <= cap);
}

#[test]
fn a_replayed_store_never_reaches_the_clipboard() {
    let mut core = AlacrittyCore::new(80, 24);
    core.write_raw(b"\x1b]52;c;TGl2ZQ==\x07");
    core.write(b"\x1b]52;c;UmVwbGF5\x07");
    assert!(
        core.take_clipboard_writes().is_empty(),
        "a replay drops the live write still queued as well as its own"
    );
}
