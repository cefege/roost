#![cfg(unix)]
//! The pure byte-stream scanners, exercised on the buffers that break them: a
//! sequence cut in half at a chunk boundary, a payload that never terminates, a
//! percent escape that is not one, and two toggles in one buffer.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_worker::session::stream_scan::{
    ALT_ENTER_SEQUENCES, MODE_CARRY_MAX, OSC7_CARRY_MAX, parse_osc7_worker_path, scan_alt_mode,
    scan_osc7,
};

/// THE LAST TOGGLE IN A BUFFER WINS. A TUI that leaves and another that enters
/// inside one chunk must leave the grid on the alternate screen, because that is
/// where the bytes now being written belong.
#[test]
fn the_last_alt_screen_toggle_in_a_buffer_wins() {
    assert!(
        !scan_alt_mode(b"a\x1b[?1049hb\x1b[?1049l", false),
        "enter then leave leaves the grid alone"
    );
    assert!(
        scan_alt_mode(b"a\x1b[?1049lb\x1b[?1049h", false),
        "leave then enter leaves the grid on the alternate screen"
    );
    assert!(
        scan_alt_mode(b"nothing to see", true),
        "and a buffer with no toggle leaves the previous answer alone"
    );
}

/// EVERY ENTER SEQUENCE IS RECOGNISED, not just the modern one, and each one is
/// found in the LAST legal position of the buffer — the bytes after it are the
/// ones whose screen it decides.
#[test]
fn every_alt_screen_enter_sequence_is_recognised() {
    for sequence in ALT_ENTER_SEQUENCES {
        let mut buffer = b"before".to_vec();
        buffer.extend_from_slice(sequence);
        assert!(
            scan_alt_mode(&buffer, false),
            "{} is an enter and was not recognised",
            String::from_utf8_lossy(sequence)
        );
        assert!(
            !scan_alt_mode(b"a\x1b[?1049l", true),
            "and a buffer whose only toggle is a leave is a leave, whatever the \
             previous answer was"
        );
    }
}

/// A MODE CARRY THAT IS TOO SHORT LOSES A SPLIT TOGGLE. The cap is the length
/// the sequence needs, not a round number, and this is what pins it.
#[test]
fn the_mode_carry_holds_the_longest_toggle() {
    let longest = ALT_ENTER_SEQUENCES
        .iter()
        .map(|sequence| sequence.len())
        .max()
        .expect("there is at least one enter sequence");
    assert_eq!(
        MODE_CARRY_MAX,
        longest - 1,
        "one byte short of the sequence is the most a partial match can be"
    );
}

/// A `file://` payload is percent-decoded, and a component that is not valid
/// percent-encoding is left exactly as it arrived rather than half-decoded. A
/// half-decoded path names a folder that does not exist, and the shell's real
/// folder is one keystroke away.
#[test]
fn an_osc7_path_is_decoded_whole_or_not_at_all() {
    assert_eq!(
        parse_osc7_worker_path("file:///home/almalinux/re%20pos"),
        Some("/home/almalinux/re pos".to_string()),
        "an ordinary escape decodes"
    );
    assert_eq!(
        parse_osc7_worker_path("file:///tmp/%zz"),
        Some("/tmp/%zz".to_string()),
        "a malformed escape leaves the component as it arrived"
    );
    assert_eq!(
        parse_osc7_worker_path("file:///tmp/%e4%b8%ad"),
        Some("/tmp/中".to_string()),
        "a custom prompt may percent-encode raw UTF-8"
    );
    assert_eq!(
        parse_osc7_worker_path("/tmp/without-a-scheme"),
        None,
        "and a payload that is not a file URL is not a folder"
    );
    assert_eq!(
        parse_osc7_worker_path("file://host/with/host"),
        Some("/with/host".to_string()),
        "a POSIX worker's path is the payload; the authority is not part of it"
    );
    assert_eq!(
        parse_osc7_worker_path("file:///tmp/%00null"),
        None,
        "and a NUL byte is not a folder, because no path contains one"
    );
}

/// A TERMINATED OSC 7 IS READ, and several in one buffer collapse to the last.
#[test]
fn a_terminated_osc7_is_read_and_the_last_one_wins() {
    assert_eq!(
        scan_osc7(b"\x1b]7;file:///one\x07").cwd.as_deref(),
        Some("/one")
    );
    assert_eq!(
        scan_osc7(b"\x1b]7;file:///one\x1b\\").cwd.as_deref(),
        Some("/one"),
        "ST terminates a report as surely as BEL"
    );
    assert_eq!(
        scan_osc7(b"\x1b]7;file:///one\x07\x1b]7;file:///two\x07")
            .cwd
            .as_deref(),
        Some("/two"),
        "several cd's in one chunk collapse to the destination the shell is at"
    );
    assert_eq!(
        scan_osc7(b"\x1b]7;file:///a/b\x1b[0m").cwd.as_deref(),
        None,
        "an ESC that is not ST means the payload was interrupted, not that the \
         folder is /a/b"
    );
}

/// AN UNTERMINATED OSC 7 IS CARRIED, NOT PARSED, AND THE CARRY IS BOUNDED. A
/// shell that opens a report and never closes it must not be read as having
/// changed folder, and the tail that could still become a report cannot pin
/// memory.
#[test]
fn an_unterminated_osc7_is_carried_and_bounded() {
    let mut combined = b"noise\x1b]7;file:///a/very/long/".to_vec();
    combined.extend(std::iter::repeat_n(b'x', 4096));
    let scan = scan_osc7(&combined);
    assert_eq!(scan.cwd, None, "no terminator, so no folder");
    assert!(
        scan.carry.len() <= OSC7_CARRY_MAX,
        "and the carry is bounded: {}",
        scan.carry.len()
    );
}

/// THE CARRY IS WHAT MAKES A SPLIT REPORT PARSE. Without it, a `cd` whose report
/// straddles two chunks is a folder the worker never learns about, and the
/// session's cwd stays one `cd` behind for the life of the shell.
#[test]
fn a_report_split_across_two_chunks_is_read_from_the_carry() {
    let first = b"\x1b]7;file:///home/almalinux/".to_vec();
    assert_eq!(
        scan_osc7(&first).cwd,
        None,
        "the first half alone names no folder"
    );
    let mut second = scan_osc7(&first).carry;
    second.extend_from_slice(b"repos/roost\x07");
    assert_eq!(
        scan_osc7(&second).cwd.as_deref(),
        Some("/home/almalinux/repos/roost"),
        "and the carry turns the second half into a complete report"
    );
}
