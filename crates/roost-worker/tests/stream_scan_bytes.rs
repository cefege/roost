//! The pure byte-stream scanners, exercised on the buffers that break them: a
//! sequence cut in half at a chunk boundary, a payload that never terminates, a
//! percent escape that is not one, and two toggles in one buffer.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_worker::session::stream_scan::{
    AGENT_OSC_CARRY_MAX, AGENT_OSC_PROGRESS_MAX, AGENT_OSC_TITLE_MAX, ALT_ENTER_SEQUENCES,
    MODE_CARRY_MAX, OSC7_CARRY_MAX, parse_osc7_worker_path, scan_agent_osc, scan_alt_mode,
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

/// A TITLE IS READ, CONTROL BYTES ARE STRIPPED. A prompt that paints a CSI
/// inside its window title set the title anyway, and the front end would render
/// the escape as glyphs.
#[test]
fn an_agent_title_is_read_and_stripped_of_control_bytes() {
    assert_eq!(
        scan_agent_osc(b"\x1b]2;my shell\x07").title.as_deref(),
        Some("my shell")
    );
    assert_eq!(
        scan_agent_osc(b"\x1b]0;clean\x1b[2Jmore\x07")
            .title
            .as_deref(),
        Some("clean[2Jmore"),
        "a CSI inside a title body does not end the sequence"
    );
    assert!(
        scan_agent_osc(b"\x1b]2;my shell\x07").carry.is_empty(),
        "and a fully-parsed buffer carries nothing"
    );
}

/// A CAPPED TITLE NEVER ENDS ON A HALF CODEPOINT. The value is user-visible text
/// a front end truncates again for display, and a cut surrogate pair is a
/// replacement character in a window title.
#[test]
fn a_capped_agent_title_never_ends_mid_code_point() {
    let long = "\u{1f600}".repeat(AGENT_OSC_TITLE_MAX);
    let mut buffer = b"\x1b]0;".to_vec();
    buffer.extend_from_slice(long.as_bytes());
    buffer.push(0x07);
    let title = scan_agent_osc(&buffer)
        .title
        .expect("a title this long is still a title");
    assert!(title.len() <= AGENT_OSC_TITLE_MAX, "{}", title.len());
    assert!(
        !title.ends_with('\u{FFFD}'),
        "a title that ends in a replacement character was cut mid-code-point"
    );

    let long_progress = "9".repeat(AGENT_OSC_PROGRESS_MAX);
    let mut progress = b"\x1b]9;4;".to_vec();
    progress.extend_from_slice(long_progress.as_bytes());
    progress.push(0x07);
    let value = scan_agent_osc(&progress)
        .progress
        .expect("a progress body this long is still a progress body");
    assert!(value.len() <= AGENT_OSC_PROGRESS_MAX);
}

/// AN UNTERMINATED TITLE IS CARRIED FROM ITS OWN `ESC ]`, bounded, and the
/// carry is exactly what the next chunk needs to complete it. That is the whole
/// point of a carry: a window title cut in half by a chunk boundary is a title
/// the agent detector never sees.
#[test]
fn an_unterminated_title_is_carried_from_its_own_start_and_completable() {
    let first = b"noise\x1b]2;my long running pro".to_vec();
    let scan = scan_agent_osc(&first);
    assert_eq!(scan.title, None, "no terminator, so no title");
    assert_eq!(
        scan.carry.first().copied(),
        Some(0x1b),
        "and the carry starts at the sequence, not at the noise before it"
    );
    let mut second = scan.carry;
    second.extend_from_slice(b"mpt\x07");
    assert_eq!(
        scan_agent_osc(&second).title.as_deref(),
        Some("my long running prompt"),
        "so the next chunk completes it into the title the prompt set"
    );
}

/// AN OSC CARRY THAT PASSES ITS CAP KEEPS THE TAIL AND ABANDONS THE SEQUENCE.
/// A stream that opens a title and never closes it past the cap must not be able
/// to pin worker memory, and the bytes it loses are the ones a client already
/// had no use for.
#[test]
fn an_agent_osc_carry_past_its_cap_keeps_the_tail() {
    let mut combined = b"noise\x1b]2;".to_vec();
    combined.extend(std::iter::repeat_n(b'x', 4096));
    let scan = scan_agent_osc(&combined);
    assert_eq!(scan.title, None, "no terminator, so no title");
    assert!(
        scan.carry.len() <= AGENT_OSC_CARRY_MAX,
        "and the carry is bounded: {}",
        scan.carry.len()
    );
    assert!(
        !scan.carry.contains(&0x1b),
        "and the abandoned sequence's introducer is dropped with the rest of it, \
         so the tail is not re-parsed as a new sequence"
    );
}

/// ONLY A LONE TRAILING ESC IS CARRIED from a fully-parsed buffer. Keeping any
/// longer tail would re-report a title the client already has.
#[test]
fn a_parsed_agent_buffer_carries_only_a_trailing_escape() {
    let scan = scan_agent_osc(b"\x1b]2;done\x07\x1b");
    assert_eq!(scan.title.as_deref(), Some("done"));
    assert_eq!(scan.carry, vec![0x1b]);
    assert!(
        scan_agent_osc(b"\x1b]2;done\x07tail").carry.is_empty(),
        "and text after the terminator is not a carry"
    );
}

/// AN OSC THE SCANNER DOES NOT CARE ABOUT MUST NOT SWALLOW THE ONE AFTER IT.
/// Shell-integration sequences sit between titles constantly, and a scanner that
/// stopped at their terminator would report the wrong window title.
#[test]
fn an_unrelated_osc_does_not_swallow_the_title_after_it() {
    let scan = scan_agent_osc(b"\x1b]133;A\x07\x1b]2;real title\x07");
    assert_eq!(
        scan.title.as_deref(),
        Some("real title"),
        "a shell-integration OSC is skipped, not treated as a title"
    );
    assert_eq!(scan.progress, None, "and 133 is not a progress report");
}

/// ONLY AN OSC 9 `4;` BODY IS PROGRESS. OSC 9 carries other notifications, and
/// reading one of those as a progress value paints a notification in the place a
/// progress bar belongs.
#[test]
fn only_an_osc_nine_four_body_is_progress() {
    assert_eq!(
        scan_agent_osc(b"\x1b]9;4;50\x07").progress.as_deref(),
        Some("50")
    );
    assert_eq!(
        scan_agent_osc(b"\x1b]9;conveyed\x07").progress,
        None,
        "OSC 9 with another body is a notification, not progress"
    );
}
