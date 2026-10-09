//! Keeper-update preparation freezes terminal writes before they reach the
//! keeper it is about to replace, and both preparation outcomes thaw them. The
//! real admission lane and preparation handler run over a scripted keeper; only
//! the maintenance action is deferred so the frozen window is observable.
//! Ports `apps/worker/tests/keeper-update-terminal-freeze.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "session_support/mod.rs"]
mod session_support;
mod terminal_stream_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use roost_proto::DKeeperUpdatePrepare;
use roost_protocol::wire::coord_worker::{TerminalStreamFailureKind, TerminalWritePhase};
use roost_term::RioCore;
use roost_worker::keeper_pool::{
    BoundaryRelease, JournaledKeeperUpdateActionV1, KeeperUpdateActionResult, KeeperUpdateActions,
    KeeperUpdateBoundary, KeeperUpdatePreparer,
};
use roost_worker::link_ports::KeeperUpdatePort;
use roost_worker::session::input_write::WorkerInputResult;
use roost_worker::session::keeper_admission::KEEPER_UPDATE_WRITE_REFUSAL;
use roost_worker::session::lifecycle::{SessionManager, SessionTable};
use roost_worker::session::terminal_state::WorkerStreamResult;
use roost_worker::uplink::OwnerFuture;
use tokio::sync::oneshot;

const INPUT: &[u8] = b"frozen-input";

#[derive(Default)]
struct Boundary {
    released: Arc<AtomicUsize>,
}

impl KeeperUpdateBoundary for Boundary {
    fn acquire(&self) -> OwnerFuture<Result<BoundaryRelease, String>> {
        let released = Arc::clone(&self.released);
        Box::pin(async move {
            Ok(Box::new(move || {
                released.fetch_add(1, Ordering::SeqCst);
            }) as BoundaryRelease)
        })
    }
}

/// The maintenance action, answered by the test.
struct Deferred(std::sync::Mutex<Option<oneshot::Receiver<Result<&'static str, String>>>>);

impl KeeperUpdateActions for Deferred {
    fn apply(
        &self,
        _action: JournaledKeeperUpdateActionV1,
    ) -> OwnerFuture<Result<KeeperUpdateActionResult, String>> {
        Box::pin(async { Err("a maintenance test never applies a journal".to_owned()) })
    }

    fn maintenance_shutdown(&self, _force_live: bool) -> OwnerFuture<Result<&'static str, String>> {
        let answer = self.0.lock().unwrap().take();
        Box::pin(async move {
            match answer {
                Some(answer) => answer.await.unwrap_or_else(|_| Err("dropped".to_owned())),
                None => Err("answered twice".to_owned()),
            }
        })
    }
}

/// The in-flight update request and the sender that settles its boundary.
type Begun = (
    tokio::task::JoinHandle<Result<serde_json::Value, String>>,
    oneshot::Sender<Result<&'static str, String>>,
);

/// A forced maintenance preparation over `session`, started, with the sender
/// that answers its keeper action.
fn begin(
    manager: &Arc<SessionManager>,
    table: &Arc<SessionTable>,
    session: &str,
    boundary: &Arc<Boundary>,
) -> Begun {
    let (answer, deferred) = oneshot::channel();
    let preparer = KeeperUpdatePreparer::new(
        Arc::clone(manager),
        Arc::clone(table),
        Arc::clone(boundary) as Arc<dyn KeeperUpdateBoundary>,
        Arc::new(Deferred(std::sync::Mutex::new(Some(deferred)))),
    );
    let request = DKeeperUpdatePrepare {
        request_id: "keeper-update-terminal-freeze".to_owned(),
        direction: String::new(),
        maintenance: true,
        force_live: true,
        coordinator_open_session_ids: vec![session.to_owned()],
        ..Default::default()
    };
    (tokio::spawn(preparer.prepare(request)), answer)
}

#[tokio::test]
async fn input_is_rejected_pre_write_with_zero_keeper_writes_and_accepted_after_rollback() {
    let harness = session_support::Harness::new();
    harness.install(
        session_support::SESSION,
        7,
        "/home/user/project",
        "/home/user/project",
    );
    let boundary = Arc::new(Boundary::default());
    let session = session_support::session_id(session_support::SESSION);
    let (preparation, answer) = begin(
        &harness.manager,
        &harness.table,
        session_support::SESSION,
        &boundary,
    );

    let frozen = harness
        .manager
        .write_terminal_input(&session, 1, INPUT.to_vec(), None, None)
        .await;
    assert_eq!(
        frozen,
        WorkerInputResult::Rejected {
            reason: KEEPER_UPDATE_WRITE_REFUSAL.to_owned()
        }
    );
    assert!(harness.keeper.input.written().is_empty());

    answer
        .send(Err("injected maintenance failure".to_owned()))
        .unwrap();
    assert_eq!(
        preparation.await.unwrap(),
        Err("injected maintenance failure".to_owned())
    );
    assert_eq!(boundary.released.load(Ordering::SeqCst), 1);

    let thawed = harness
        .manager
        .write_terminal_input(&session, 2, INPUT.to_vec(), None, None)
        .await;
    assert_eq!(
        thawed,
        WorkerInputResult::Accepted {
            written_bytes: INPUT.len() as u32
        }
    );
    assert_eq!(harness.keeper.input.written().len(), 1);
}

#[tokio::test]
async fn a_stream_resize_is_refused_without_mutating_stream_state_and_success_thaws_writes() {
    use terminal_stream_support::{COLS, Harness, ROWS, SESSION, STREAM_A, channel, held};
    let harness = Harness::scripted(RioCore::new(COLS, ROWS));
    let boundary = Arc::new(Boundary::default());
    let (preparation, answer) = begin(&harness.manager, &harness.table, SESSION, &boundary);

    let refused = harness
        .manager
        .apply_terminal_stream_state(harness.intent(STREAM_A, true, 40, 10))
        .await;
    assert!(
        matches!(&refused, WorkerStreamResult::Rejected { reason, .. } if reason == KEEPER_UPDATE_WRITE_REFUSAL),
        "{refused:?}"
    );
    assert_eq!(
        refused.failure(),
        Some(TerminalStreamFailureKind::RetryablePreWrite)
    );
    assert_eq!(refused.phase(), TerminalWritePhase::PreWrite);
    assert!(
        held(&harness.keeper.resized).is_empty(),
        "no resize reached the keeper"
    );
    assert!(
        harness.manager.terminal_stream_facts(channel()).is_none(),
        "no stream was installed"
    );

    answer.send(Ok("shutdown")).unwrap();
    assert_eq!(
        preparation.await.unwrap(),
        Ok(serde_json::json!({ "outcome": "shutdown" }))
    );

    let thawed = harness
        .manager
        .apply_terminal_stream_state(harness.intent(STREAM_A, true, 40, 10))
        .await;
    assert!(
        !matches!(thawed, WorkerStreamResult::Rejected { .. }),
        "{thawed:?}"
    );
    assert_eq!(
        held(&harness.keeper.resized).len(),
        1,
        "the thawed resize reached the keeper"
    );
}
