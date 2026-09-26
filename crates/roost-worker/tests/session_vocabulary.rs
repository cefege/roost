//! The session record's own arithmetic: the monotonic byte offset, the history
//! floor eviction moves, and the state a record is born in. Every W-1 slice
//! reads these numbers, so they are pinned here rather than inside whichever
//! slice happened to touch one first.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_host::HostPlatform;
use roost_term::{AlacrittyCore, CellEmitState};
use roost_worker::event_store::{DurableEventKind, Store};
use roost_worker::session::agent_osc::AgentOscState;
use roost_worker::session::ring::ScrollbackRing;
use roost_worker::session::types::{SessionIdentity, SessionRecord};
use roost_worker::shell_spec::ShellSpec;

/// A record with the given retained-window size, so a test reaches eviction in
/// bytes rather than in megabytes. The window is a parameter rather than the
/// default because these tests are about the arithmetic, not about the cap.
fn record(window: usize) -> SessionRecord {
    let mut store = Store::new();
    let reservation = store
        .reserve(DurableEventKind::Closed, 64)
        .expect("the store admits a close claim");
    SessionRecord::new(
        SessionIdentity {
            session_id: "6f1c0f2e-1f4a-4a3e-9b2f-0a1b2c3d4e5f"
                .try_into()
                .expect("a uuid is a session id"),
            channel_id: 7i64.try_into().expect("a positive id is a channel id"),
            socket_path: "/run/roost/mux-keeper.sock".to_string(),
            cwd: "/home/almalinux/repos/roost".to_string(),
            shell_spec: ShellSpec {
                version: 1,
                platform: HostPlatform::Linux,
                executable: "/bin/bash".to_string(),
                argv: vec!["--rcfile".to_string(), "/tmp/roost-bootstrap".to_string()],
                cwd: "/home/almalinux/repos/roost".to_string(),
                env: vec![("TERM".to_string(), "xterm-256color".to_string())],
            },
            session_trace_id: "aabbccdd11223344".try_into().expect("hex is a trace id"),
            spawned_at_ms: 1_700_000_000_000,
        },
        reservation,
        Box::new(AlacrittyCore::new(80, 24)),
        CellEmitState::new("epoch-1", "stream-1"),
        ScrollbackRing::new(window),
    )
}

/// A fresh session has produced nothing, so its floor is zero and the dead-birth
/// check must not fire on it.
///
/// `produced_output` is what tells a stillborn child from a shell that exited
/// quickly, and a record that reported `true` before its first byte would make
/// every fast `exit` look like a keeper that needs restarting.
#[test]
fn a_new_session_has_produced_nothing_and_its_floor_is_zero() {
    let session = record(64);
    assert_eq!(session.head_seq, 0);
    assert_eq!(session.history_floor(), 0);
    assert!(!session.produced_output());
    assert!(session.scrollback.is_empty());
}

/// The offset counts every byte the session produced, and the floor is what
/// eviction left behind.
///
/// The two numbers are different on purpose: a client addresses retained history
/// by an absolute monotonic index, so the counter must keep counting past the
/// point the window stops holding bytes. A counter that stopped at the retained
/// length would re-alias every row a browser still holds, invisibly.
#[test]
fn the_history_floor_is_the_offset_before_the_oldest_retained_byte() {
    let mut session = record(8);
    session.append_retained(b"abcdefgh");
    assert_eq!(session.head_seq, 8);
    assert_eq!(session.scrollback.len(), 8);
    assert_eq!(session.history_floor(), 0);
    assert_eq!(session.scrollback.to_vec(), b"abcdefgh".to_vec());

    // Four more bytes evict the first four. The offset is 12; the window holds
    // eight of them; the floor is therefore 4 — the offset of the byte before
    // `e`.
    let end = session.append_retained(b"ijkl");
    assert_eq!(end, 12);
    assert_eq!(session.scrollback.len(), 8);
    assert_eq!(session.history_floor(), 4);
    assert_eq!(session.scrollback.to_vec(), b"efghijkl".to_vec());
    assert_eq!(
        session.history_floor() + session.scrollback.len() as u64,
        session.head_seq
    );
}

/// A chunk larger than the whole window still counts every byte it produced.
///
/// This branch exists because the obvious implementation — evict byte by byte
/// until the chunk fits — is correct and quadratic, and a paste of a megabyte
/// into a small window is something a caller writes by accident. The invariant
/// that has to survive the fast path is the same one the general path keeps.
#[test]
fn a_chunk_larger_than_the_window_retains_only_its_own_tail() {
    let mut session = record(8);
    let end = session.append_retained(b"0123456789abcdef");
    assert_eq!(end, 16);
    assert_eq!(session.scrollback.len(), 8);
    assert_eq!(session.scrollback.to_vec(), b"89abcdef".to_vec());
    assert_eq!(session.history_floor(), 8);
    assert!(session.scrollback.evicting());
}

/// An adopted session's history belongs to a process that is no longer this
/// record's, so the offset comes from the keeper rather than from counting what
/// arrives here.
///
/// The failure this pins is the adoption frame being wrong from its first byte:
/// a floor left at zero makes every absolute row index a browser adopts re-alias
/// onto history the window never held.
#[test]
fn an_adopted_session_takes_the_keeper_head_and_its_own_floor() {
    let mut session = record(8);
    // A keeper that has produced 5000 bytes and still retains its last 8.
    session.adopt_retained_history(b"89abcdef", 5_000);
    assert_eq!(session.head_seq, 5_000);
    assert_eq!(session.history_floor(), 4_992);
    assert_eq!(session.scrollback.to_vec(), b"89abcdef".to_vec());
    assert_eq!(
        session.history_floor() + session.scrollback.len() as u64,
        session.head_seq
    );

    // Output continues from the adopted offset, not from the retained length.
    let end = session.append_retained(b"g");
    assert_eq!(end, 5_001);
    assert_eq!(session.history_floor(), 4_993);
}

/// A record is born on the primary screen, with no half-read escape sequence and
/// an emitter that has shipped nothing.
///
/// Each of these is a field a slice reads before it has written to it. A record
/// that started on the alt screen would ship an empty history replay as if it
/// were a TUI, and one whose emitter claimed a full frame would leave a client
/// believing it already holds a grid it was never sent.
#[test]
fn a_record_starts_on_the_primary_screen_with_nothing_shipped() {
    let session = record(64);
    assert!(!session.alt_mode);
    assert!(session.mode_carry.is_empty());
    assert!(session.osc7_carry.is_empty());
    assert!(session.query_carry.is_empty());
    assert_eq!(session.last_pty_out_ms, 0);
    assert!(session.sb_origin_pin.is_none());
    assert!(session.unhandled.is_none());
    assert!(!session.cell_emit.sent_full);
    assert_eq!(session.cell_emit.seq, 0);
    assert_eq!(session.identity.shell_spec.cwd, session.identity.cwd);

    // A session has resolved nothing about its folder yet, and "not looked
    // yet" is a different answer from "looked, and there is nothing there" —
    // so the OUTER option is what a client checks to decide whether to render
    // a branch at all, and collapsing the two would make every unresolvable
    // folder look like a deliberate "not a repository".
    assert!(session.git_branch.is_none());
    assert!(session.git_remote.is_none());
    assert!(session.pr.is_none());
    assert!(session.child_pid.is_none());
    assert!(session.ports.is_none());
}

/// Replacing an agent clears the evidence the dead process left, and keeps the
/// half-read sequence the new one may have started.
///
/// Dropping the carry as well would glue the tail of the OLD process's OSC
/// sequence onto the front of the NEW process's first chunk and answer a probe
/// nobody sent; keeping the title would judge the new process by the old one's
/// final one.
#[test]
fn replacing_an_agent_clears_its_title_but_keeps_the_split_sequence() {
    let mut evidence = AgentOscState {
        carry: b"\x1b]0;half".to_vec(),
        raw_title: "codex - old".to_string(),
        raw_progress: "50%".to_string(),
    };
    evidence.clear_evidence();
    assert!(evidence.raw_title.is_empty());
    assert!(evidence.raw_progress.is_empty());
    assert_eq!(evidence.carry, b"\x1b]0;half".to_vec());
}
