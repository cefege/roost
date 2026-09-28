//! The session layer's input truth mapping over a scripted keeper: what is
//! rejected (provably unwritten), what is ambiguous (written, outcome unknown),
//! and the live-authority recheck after the write-ordering lane is granted.
//! Ports `apps/worker/tests/terminal/terminal-stream-input.test.ts` (the
//! authority and worker-owned cases; keeper-key correlation is pinned against a
//! real keeper in `terminal_input_e2e.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "session_support/mod.rs"]
mod session_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use roost_worker::session::input_write::{
    TerminalWriteAuthority, TerminalWriteBudget, WorkerInputResult,
};
use roost_worker::session::keeper_admission::{Admission, AdmissionKind};
use roost_worker::session::keeper_channels::{InputNotWritten, KeeperInputResult};
use session_support::input_script::ScriptedAnswer;
use session_support::{Harness, SESSION, channel, session_id};

const CHANNEL: u16 = 7;

fn harness() -> Harness {
    let harness = Harness::new();
    harness.install(SESSION, CHANNEL, "/home/user/project", "/home/user/project");
    harness
}

struct Flags {
    authorized: Arc<AtomicBool>,
    route: Arc<AtomicBool>,
}

impl TerminalWriteAuthority for Flags {
    fn is_session_authorized(&self) -> bool {
        self.authorized.load(Ordering::SeqCst)
    }
    fn is_current_input_route(&self) -> bool {
        self.route.load(Ordering::SeqCst)
    }
}

struct Budget {
    current: bool,
    expired: bool,
}

impl TerminalWriteBudget for Budget {
    fn is_current_connection(&self) -> bool {
        self.current
    }
    fn expired(&self) -> bool {
        self.expired
    }
}

fn rejected(reason: &str) -> WorkerInputResult {
    WorkerInputResult::Rejected {
        reason: reason.to_owned(),
    }
}

#[tokio::test]
async fn worker_owned_input_is_written_and_accepted_without_browser_authority() {
    let harness = harness();
    let result = harness
        .manager
        .write_worker_owned_input(&session_id(SESSION), b"W".to_vec())
        .await;
    assert_eq!(result, WorkerInputResult::Accepted { written_bytes: 1 });
    assert_eq!(
        harness.keeper.input.written(),
        vec![(CHANNEL, b"W".to_vec())]
    );
}

/// THE TRUTH MAPPING'S LOAD-BEARING CASE: once the request reached the socket,
/// a lost answer is ambiguous. Reporting it rejected would let the browser
/// resend a keystroke the PTY may already have.
#[tokio::test]
async fn a_written_batch_whose_answer_is_lost_is_ambiguous_never_rejected() {
    let harness = harness();
    harness
        .keeper
        .input
        .answer_next(ScriptedAnswer::Answered(KeeperInputResult::Ambiguous {
            written: None,
            reason: "disconnected".to_owned(),
        }));
    let result = harness
        .manager
        .write_terminal_input(&session_id(SESSION), 1, b"ab".to_vec(), None, None)
        .await;
    assert_eq!(
        result,
        WorkerInputResult::Ambiguous {
            written_bytes: 0,
            reason: "disconnected".to_owned()
        }
    );
    harness
        .keeper
        .input
        .answer_next(ScriptedAnswer::Answered(KeeperInputResult::Ack {
            written: 1,
        }));
    let short = harness
        .manager
        .write_terminal_input(&session_id(SESSION), 2, b"cd".to_vec(), None, None)
        .await;
    assert!(
        matches!(
            short,
            WorkerInputResult::Ambiguous {
                written_bytes: 1,
                ..
            }
        ),
        "an incomplete ack is not accepted"
    );
}

#[tokio::test]
async fn only_a_request_that_never_reached_the_socket_or_a_keeper_refusal_is_rejected() {
    let harness = harness();
    harness
        .keeper
        .input
        .answer_next(ScriptedAnswer::NotWritten(InputNotWritten::QueueFull));
    let unwritten = harness
        .manager
        .write_terminal_input(&session_id(SESSION), 1, b"a".to_vec(), None, None)
        .await;
    assert_eq!(
        unwritten,
        rejected("keeper did not accept the input: queue_full")
    );
    harness
        .keeper
        .input
        .answer_next(ScriptedAnswer::Answered(KeeperInputResult::Reject {
            reason: "channel_exited".to_owned(),
        }));
    let refused = harness
        .manager
        .write_terminal_input(&session_id(SESSION), 2, b"b".to_vec(), None, None)
        .await;
    assert_eq!(refused, rejected("channel_exited"));
    assert_eq!(
        harness
            .manager
            .write_terminal_input(&session_id(SESSION), 0, b"c".to_vec(), None, None)
            .await,
        rejected("input sequence must be positive")
    );
    let empty = harness
        .manager
        .write_terminal_input(&session_id(SESSION), 3, Vec::new(), None, None)
        .await;
    assert_eq!(empty, WorkerInputResult::Accepted { written_bytes: 0 });
    assert_eq!(
        harness.keeper.input.written(),
        vec![(CHANNEL, b"b".to_vec())]
    );
}

/// The v2 fence against a delayed old-route write: authority is checked
/// again after the lane is granted, immediately before the keeper write.
#[tokio::test]
async fn live_authority_is_rechecked_after_the_lane_is_granted() {
    let harness = harness();
    let lanes = Arc::clone(harness.manager.control_lanes());
    for (authorized, route, reason) in [
        (true, false, "terminal input route changed"),
        (false, true, "terminal session is unavailable"),
    ] {
        let Admission::Granted(blocker) =
            lanes.admit(channel(i64::from(CHANNEL)), AdmissionKind::TerminalResize)
        else {
            panic!("the resize lane admits");
        };
        blocker.granted().await;
        let flags = Flags {
            authorized: Arc::new(AtomicBool::new(true)),
            route: Arc::new(AtomicBool::new(true)),
        };
        let (live_authorized, live_route) =
            (Arc::clone(&flags.authorized), Arc::clone(&flags.route));
        let pending = tokio::spawn(harness.manager.write_terminal_input(
            &session_id(SESSION),
            1,
            b"R".to_vec(),
            None,
            Some(Box::new(flags)),
        ));
        tokio::task::yield_now().await;
        live_authorized.store(authorized, Ordering::SeqCst);
        live_route.store(route, Ordering::SeqCst);
        blocker.release();
        assert_eq!(pending.await.unwrap(), rejected(reason));
    }
    assert!(
        harness.keeper.input.written().is_empty(),
        "a fenced write never reaches the keeper"
    );
}

#[tokio::test]
async fn the_request_budget_is_honoured_before_queued_keeper_work() {
    let harness = harness();
    for (budget, reason) in [
        (
            Budget {
                current: false,
                expired: false,
            },
            "worker connection superseded before the keeper write",
        ),
        (
            Budget {
                current: true,
                expired: true,
            },
            "input budget expired before the keeper write",
        ),
    ] {
        let result = harness
            .manager
            .write_terminal_input(
                &session_id(SESSION),
                1,
                b"x".to_vec(),
                Some(Box::new(budget)),
                None,
            )
            .await;
        assert_eq!(result, rejected(reason));
    }
    let lanes = harness.manager.control_lanes();
    lanes.set_keeper_update_prepared(true);
    let frozen = harness
        .manager
        .write_worker_owned_input(&session_id(SESSION), b"x".to_vec())
        .await;
    assert!(
        matches!(frozen, WorkerInputResult::Rejected { .. }),
        "a prepared keeper replacement refuses input"
    );
    assert!(harness.keeper.input.written().is_empty());
}

#[tokio::test]
async fn legacy_binary_input_reaches_the_keeper_only_for_a_held_channel() {
    let harness = harness();
    harness.manager.write_legacy_input(CHANNEL, b"k");
    harness.manager.write_legacy_input(CHANNEL + 1, b"lost");
    assert_eq!(
        harness.keeper.input.legacy(),
        vec![(CHANNEL, b"k".to_vec())]
    );
}
