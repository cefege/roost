//! Append, replay and the unhandled-sequence log: the policy that decides which
//! bytes a session keeps and what a chunk tells the worker about the stream. The
//! arithmetic being guarded here is the one whose breakage is invisible — a
//! re-aliased row index, a half-recognised probe, a diagnostic that claims a
//! completeness it does not have.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_host::HostPlatform;
use roost_term::AlacrittyCore;
use roost_worker::event_store::{DurableEventKind, Store};
use roost_worker::session::history::{UNHANDLED_SEQ_MAX, UnhandledSequenceEntry};
use roost_worker::session::ring::ScrollbackRing;
use roost_worker::session::scrollback::{append_pty_chunk, record_unhandled, replay_retained_into};
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

fn observed(final_byte: &str, private: &str, params: Vec<u32>, at: u64) -> UnhandledSequenceEntry {
    UnhandledSequenceEntry {
        final_byte: final_byte.to_string(),
        private: private.to_string(),
        param_count: params.len() as u32,
        params,
        first_seen_mono_ms: at,
    }
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
        head = append_pty_chunk(&mut session, chunk, &mut |_| {});
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
    append_pty_chunk(&mut session, b"before\x1b[?10", &mut |_| {});
    assert!(!session.alt_mode, "a partial toggle is not a toggle");
    append_pty_chunk(&mut session, b"49hafter", &mut |_| {});
    assert!(
        session.alt_mode,
        "the carried prefix completes the sequence"
    );
    append_pty_chunk(&mut session, b"still in the alt screen", &mut |_| {});
    assert!(
        session.alt_mode,
        "and a chunk carrying no toggle does not clear a mode a TUI entered"
    );
    append_pty_chunk(&mut session, b"\x1b[?1049lback", &mut |_| {});
    assert!(!session.alt_mode, "leaving is a toggle too");
}

/// THE LAST `cd` IN A CHUNK WINS, and a folder is only reported when it
/// actually changed. Several shells emit two reports in one chunk, and a client
/// that is told about the first one paints a cwd the shell has already left.
#[test]
fn a_cwd_change_is_reported_once_and_only_when_it_changed() {
    let mut session = record(1024);
    let mut seen: Vec<String> = Vec::new();
    let mut record_change = |cwd: &str| seen.push(cwd.to_string());

    append_pty_chunk(
        &mut session,
        b"\x1b]7;file:///a\x07\x1b]7;file:///b\x07",
        &mut record_change,
    );
    assert_eq!(seen, vec!["/b".to_string()], "the final destination");
    assert_eq!(session.identity.cwd, "/b");

    seen.clear();
    append_pty_chunk(&mut session, b"just some output", &mut record_change);
    assert!(
        seen.is_empty(),
        "a chunk with no report does not re-announce the folder"
    );

    append_pty_chunk(&mut session, b"\x1b]7;file:///b\x07", &mut record_change);
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
    assert!(
        scan_alt_mode(b"a\x1b[?1049hb\x1b[?1049l", false),
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

/// A REPLACEMENT CORE IS FED THE WHOLE WINDOW, OLDEST FIRST, and the caller is
/// told whether the window was at capacity. That last fact is the difference
/// between "your history aged out" and "a rebuild rebuilt less of it", and a
/// client renders the two differently.
#[test]
fn a_replay_feeds_the_whole_window_and_says_whether_it_was_saturated() {
    let mut session = record(8);
    append_pty_chunk(&mut session, b"first-four", &mut |_| {});
    append_pty_chunk(&mut session, b"last-four", &mut |_| {});
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
    let _ = replay_retained_into(&mut rebuilt, &session);
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

/// THE UNHANDLED LOG IS A WATERMARK, NOT A SCAN. The core's ring is never
/// cleared, so a second sample that re-reported what the first already recorded
/// would fill a bounded diagnostic with repeats of one novel sequence.
#[test]
fn an_unhandled_sequence_is_recorded_once_per_core_instance() {
    let mut log = None;
    let first = record_unhandled(&mut log, 1, vec![observed("q", "?", vec![2026], 10)], 0, 10);
    assert_eq!(first.recorded, 1);
    assert!(!first.capped);

    let again = record_unhandled(&mut log, 1, vec![observed("q", "?", vec![2026], 11)], 0, 11);
    assert_eq!(again.recorded, 0, "already reported");
    assert_eq!(again.repeated, 1);
    let stored = log
        .as_ref()
        .expect("a core that logged something has a log");
    assert_eq!(stored.entries.len(), 1);
    assert_eq!(stored.consumed, 1, "the watermark moved once");
}

/// A SEQUENCE WITH THE SAME FINAL BYTE BUT DIFFERENT PARAMETERS IS A DIFFERENT
/// SEQUENCE. Collapsing them would hide a novel one behind a familiar name,
/// which is the whole reason the diagnostic exists.
#[test]
fn two_sequences_with_one_final_byte_are_two_sequences() {
    let mut log = None;
    record_unhandled(
        &mut log,
        2,
        vec![
            observed("q", "?", vec![2026], 10),
            observed("q", "?", vec![1], 11),
        ],
        0,
        11,
    );
    let stored = log.as_ref().expect("two distinct sequences were recorded");
    assert_eq!(stored.entries.len(), 2);
}

/// THE LOG IS BOUNDED. A terminal that emits a novel sequence per frame would
/// otherwise grow a record without limit, and a diagnostic surface that can
/// itself be the outage is no diagnostic at all.
#[test]
fn the_unhandled_log_stops_at_its_cap_and_says_it_did() {
    let mut log = None;
    let distinct = (0..UNHANDLED_SEQ_MAX + 5)
        .map(|index| observed("q", "?", vec![index], u64::from(index)))
        .collect::<Vec<_>>();
    let summary = record_unhandled(&mut log, u64::from(UNHANDLED_SEQ_MAX) + 5, distinct, 0, 1);
    assert_eq!(summary.recorded, UNHANDLED_SEQ_MAX as u32);
    assert!(summary.capped, "and the cap is reported, not hidden");
    let stored = log.as_ref().expect("the log exists");
    assert_eq!(stored.entries.len(), UNHANDLED_SEQ_MAX);
    assert!(stored.capped);
}

/// A CORE WHOSE OWN RING OVERWROTE SEQUENCES BETWEEN SAMPLES LOSES THEM, and
/// only their existence is knowable. Recording that as zero would make the log
/// claim a completeness it does not have.
#[test]
fn sequences_the_core_ring_overwrote_are_counted_not_invented() {
    let mut log = None;
    record_unhandled(&mut log, 5, Vec::new(), 3, 1);
    let stored = log.as_ref().expect("a dropped sample still moves the log");
    assert_eq!(stored.ring_dropped, 3);
    assert!(stored.entries.is_empty());
    assert_eq!(
        stored.consumed, 5,
        "and the watermark still advances, or the same losses are counted twice"
    );
}

/// A CORE THAT HAS REPORTED NOTHING MUST COST NOTHING. The log is `None` until
/// a sample actually records something, because a healthy terminal is the case
/// and it should not allocate.
#[test]
fn a_core_that_reports_nothing_leaves_no_log() {
    let mut log = None;
    let summary = record_unhandled(&mut log, 0, Vec::new(), 0, 0);
    assert_eq!(summary.recorded, 0);
    assert!(
        log.is_none(),
        "a terminal that has never reported an unhandled sequence allocates nothing"
    );
}

/// A cwd change is a STATE TRANSITION, so the caller is told about it through a
/// seam it owns rather than the record reaching for a sink. Two reports in one
/// chunk still produce one change.
#[test]
fn a_cwd_change_reaches_the_caller_exactly_once_per_change() {
    let mut session = record(1024);
    let changes = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&changes);
    let mut report = move |cwd: &str| sink.lock().expect("held").push(cwd.to_string());
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
