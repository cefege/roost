//! Append and replay: the policy that decides which
//! bytes a session keeps and what a chunk tells the worker about the stream. The
//! arithmetic being guarded here is the one whose breakage is invisible — a
//! re-aliased row index, a half-recognised sequence.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_host::HostPlatform;
use roost_protocol::wire::brand::SessionId;
use roost_term::{AlacrittyCore, TerminalCore};
use roost_worker::event_store::{DurableEventKind, Store};
use roost_worker::session::ring::ScrollbackRing;
use roost_worker::session::scrollback::{append_pty_chunk, replay_retained_into};
use roost_worker::session::stream_scan::{parse_osc7_worker_path, scan_alt_mode, scan_osc7};
use roost_worker::session::types::{SessionIdentity, SessionRecord};
use roost_worker::shell_spec::ShellSpec;
use std::sync::{Arc, Mutex};

const SESSION: &str = "00000000-0000-4000-8000-00000000000a";

fn record(window: usize) -> SessionRecord {
    let mut store = Store::new();
    let reservation = store
        .reserve(DurableEventKind::Closed, 64)
        .expect("the store admits a close claim");
    SessionRecord::new(
        SessionIdentity {
            session_id: SESSION.try_into().expect("a uuid is a session id"),
            channel_id: 7i64.try_into().expect("a positive id is a channel id"),
            socket_path: "/run/roost/mux-keeper.sock".to_string(),
            cwd: "/home/almalinux/repos/roost".to_string(),
            shell_spec: ShellSpec {
                version: 1,
                platform: HostPlatform::Linux,
                executable: "/bin/bash".to_string(),
                argv: Vec::new(),
                cwd: "/home/almalinux/repos/roost".to_string(),
                env: vec![("TERM".to_string(), "xterm-256color".to_string())],
            },
            session_trace_id: "aabbccdd11223344".try_into().expect("hex is a trace id"),
            spawned_at_ms: 1_700_000_000_000,
        },
        reservation,
        Box::new(AlacrittyCore::new(80, 24)),
        roost_term::CellEmitState::new("epoch-1", "stream-1"),
        ScrollbackRing::new(window),
    )
}

/// EVERY retained byte advances the monotonic offset, including the ones the
/// window has already evicted. A counter that stopped at the retained length
/// would re-alias every absolute row index a browser still holds — and it would
/// look correct right up until the ring saturated.
#[test]
fn an_append_advances_the_offset_past_what_the_window_kept() {
    let mut session = record(8);
    let mut head = 0;
    for chunk in [b"abc".as_slice(), b"def", b"ghi"] {
        head = append_pty_chunk(&mut session, chunk, &mut |_, _| {});
    }
    assert_eq!(head, 9, "nine bytes were produced");
    assert_eq!(session.head_seq, 9);
    assert_eq!(session.scrollback.len(), 8, "the window kept eight");
    assert_eq!(session.history_floor(), 1, "and evicted the first");
    assert_eq!(
        session.history_floor() + session.scrollback.len() as u64,
        session.head_seq,
        "floor plus retained is the head, which is what makes an absolute \
         address mean the same thing after eviction"
    );
}

/// AN ALT-SCREEN TOGGLE SPLIT ACROSS TWO CHUNKS IS RECOGNISED EXACTLY ONCE.
/// A TUI that entered the alternate screen and had its entry sequence cut in
/// half would leave a rebuilt core painting redraws onto scrollback rows, and
/// the symptom is a terminal that scrolls when it should not.
#[test]
fn an_alt_screen_toggle_split_across_chunks_is_recognised_once() {
    let mut session = record(1024);
    append_pty_chunk(&mut session, b"before\x1b[?10", &mut |_, _| {});
    assert!(!session.alt_mode, "a partial toggle is not a toggle");
    append_pty_chunk(&mut session, b"49hafter", &mut |_, _| {});
    assert!(
        session.alt_mode,
        "the carried prefix completes the sequence"
    );
    append_pty_chunk(&mut session, b"still in the alt screen", &mut |_, _| {});
    assert!(
        session.alt_mode,
        "and a chunk carrying no toggle does not clear a mode a TUI entered"
    );
    append_pty_chunk(&mut session, b"\x1b[?1049lback", &mut |_, _| {});
    assert!(!session.alt_mode, "leaving is a toggle too");
}

/// THE LAST `cd` IN A CHUNK WINS, and a folder is only reported when it
/// actually changed. Several shells emit two reports in one chunk, and a client
/// that is told about the first one paints a cwd the shell has already left.
#[test]
fn a_cwd_change_is_reported_once_and_only_when_it_changed() {
    let mut session = record(1024);
    let mut seen: Vec<String> = Vec::new();
    {
        // Scoped, because a closure that captures `seen` mutably holds that
        // borrow for its whole lifetime — and this test has to read `seen`
        // BETWEEN appends. The block ends the borrow at the right place
        // instead of leaving the assertion to fight the closure.
        let mut record_change = |_: &SessionId, cwd: &str| seen.push(cwd.to_string());
        append_pty_chunk(
            &mut session,
            b"\x1b]7;file:///a\x07\x1b]7;file:///b\x07",
            &mut record_change,
        );
    }
    assert_eq!(seen, vec!["/b".to_string()], "the final destination");
    assert_eq!(session.identity.cwd, "/b");

    seen.clear();
    {
        let mut record_change = |_: &SessionId, cwd: &str| seen.push(cwd.to_string());
        append_pty_chunk(&mut session, b"just some output", &mut record_change);
    }
    assert!(
        seen.is_empty(),
        "a chunk with no report does not re-announce the folder"
    );

    {
        let mut record_change = |_: &SessionId, cwd: &str| seen.push(cwd.to_string());
        append_pty_chunk(&mut session, b"\x1b]7;file:///b\x07", &mut record_change);
    }
    assert!(
        seen.is_empty(),
        "and a report of the folder the session is already in is not a change"
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
        "a custom prompt may emit raw UTF-8 percent-encoded, or not"
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
}

/// AN UNTERMINATED OSC 7 IS CARRIED, NOT PARSED. A shell that opens a report and
/// never closes it must not be read as having changed folder, and the tail that
/// could still become a report is bounded so it cannot pin memory.
#[test]
fn an_unterminated_osc7_is_carried_and_bounded() {
    let mut combined = b"noise\x1b]7;file:///a/very/long/".to_vec();
    combined.extend(std::iter::repeat_n(b'x', 4096));
    let scan = scan_osc7(&combined);
    assert_eq!(scan.cwd, None, "no terminator, so no folder");
    assert!(
        scan.carry.len() <= 1024,
        "and the carry is bounded: {}",
        scan.carry.len()
    );
}

/// THE LAST TOGGLE IN A BUFFER WINS. A TUI that leaves and another that enters
/// inside one chunk must leave the grid on the alternate screen, because that is
/// where the bytes now being written belong.
#[test]
fn the_last_alt_screen_toggle_in_a_buffer_wins() {
    // Enter-then-leave ends on the PRIMARY screen, so the answer is `false`;
    // v2's `_scanAltModeTransitions` (`terminal-stream-scan.ts:55`) takes the
    // last toggle's direction the same way.
    assert!(
        !scan_alt_mode(b"a\x1b[?1049hb\x1b[?1049l", false),
        "enter then leave leaves the grid on the primary screen"
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

/// A REPLACEMENT CORE IS FED THE WHOLE WINDOW, OLDEST FIRST, and the caller is
/// told whether the window was at capacity. That last fact is the difference
/// between "your history aged out" and "a rebuild rebuilt less of it", and a
/// client renders the two differently.
#[test]
fn a_replay_feeds_the_whole_window_and_says_whether_it_was_saturated() {
    let mut session = record(8);
    append_pty_chunk(&mut session, b"first-four", &mut |_, _| {});
    append_pty_chunk(&mut session, b"last-four", &mut |_, _| {});
    assert!(
        session.scrollback.evicting(),
        "an eight-byte window is full"
    );

    let mut core = AlacrittyCore::new(20, 4);
    let replay = replay_retained_into(&mut core, &session);
    assert_eq!(replay.bytes, 8, "the window held eight bytes");
    assert_eq!(replay.head_seq, session.head_seq);
    assert!(
        replay.evicted,
        "and the caller is told the window was at capacity, because rows the \
         old core held may not exist in this one"
    );
    let mut rebuilt = AlacrittyCore::new(20, 4);
    // Not bound, and not `let _ =`: `Replay` is not `#[must_use]`, so a
    // binding would exist only to stop the compiler asking about a value this
    // test does not assert on. The summary IS pinned, by the `replay`
    // assertions above; what this second call is here for is the geometry of
    // the core after it.
    replay_retained_into(&mut rebuilt, &session);
    assert_eq!(
        core.cols(),
        rebuilt.cols(),
        "a replay is a byte-for-byte reproduction, not a geometry change"
    );

    let mut empty = record(1024);
    let mut fresh = AlacrittyCore::new(20, 4);
    let idle = replay_retained_into(&mut fresh, &empty);
    assert_eq!(idle.bytes, 0);
    assert!(!idle.evicted, "an unsaturated window lost nothing");
    empty.append_retained(b"x");
    assert!(empty.produced_output());
}

/// A cwd change is a STATE TRANSITION, so the caller is told about it through a
/// seam it owns rather than the record reaching for a sink. Two reports in one
/// chunk still produce one change.
#[test]
fn a_cwd_change_reaches_the_caller_exactly_once_per_change() {
    let mut session = record(1024);
    let changes = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&changes);
    let mut report =
        move |_: &SessionId, cwd: &str| sink.lock().expect("held").push(cwd.to_string());
    append_pty_chunk(
        &mut session,
        b"\x1b]7;file:///one\x07\x1b]7;file:///two\x07",
        &mut report,
    );
    append_pty_chunk(&mut session, b"\x1b]7;file:///two\x07", &mut report);
    append_pty_chunk(&mut session, b"\x1b]7;file:///three\x07", &mut report);
    let reported = changes.lock().expect("held").clone();
    assert_eq!(
        reported,
        vec!["/two".to_string(), "/three".to_string()],
        "one entry per actual change, not per report"
    );
}
