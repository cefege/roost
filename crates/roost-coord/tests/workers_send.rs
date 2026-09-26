// The coordinator→worker send path: the browser command, the reap, and the
// respawn a reconnect offers.
//
// These are the frames a worker executes rather than stores, so every assertion
// here is about what reached the socket and what the socket was told about the
// frame -- a send that is silently dropped and a send that is refused look the
// same on the wire, and only one of them is safe to retry.

mod workers_support;

use std::sync::Arc;

use roost_coord::workers::respawn::{
    RESPAWN_UNWATCHED_COLS, RESPAWN_UNWATCHED_ROWS, respawn_missing_for_worker,
};
use roost_coord::workers::send::{
    SendOutcome, SendRefusal, reap_orphan_pty, send_browser_command, send_frame,
};
use roost_coord::write_gate::WriteGate;
use roost_protocol::viewport::TerminalGeometry;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use roost_protocol::wire::{SessionId, WorkerFp};

use workers_support::{RecordingSocket, SESSION_ID, WORKER_FP, WorkersFixture};

fn browser_fp() -> WorkerFp {
    WorkerFp::try_from(WORKER_FP).expect("a fingerprint the brand accepts")
}

fn session_id() -> SessionId {
    SessionId::try_from(SESSION_ID).expect("a session id")
}

fn a_kill() -> ClientControlFrame {
    ClientControlFrame::Kill {
        session_id: session_id(),
        trace_id: None,
    }
}

/// A command for a worker with no socket is refused by name, not by a bare
/// `false`: "offline" and "superseded" are different bugs for whoever reads the
/// log line.
#[tokio::test]
async fn a_command_for_an_offline_worker_is_refused_by_name() {
    let fixture = WorkersFixture::new("offline").await;
    let outcome = send_browser_command(
        fixture.registry(),
        &browser_fp(),
        "browser-1",
        "viewer-1",
        "request-1",
        a_kill(),
    );
    assert_eq!(
        outcome,
        SendOutcome::Refused(SendRefusal::NoRoutableGeneration {
            worker_fp: browser_fp()
        })
    );
    assert_eq!(
        refusal_text(&outcome),
        format!("worker {WORKER_FP} has no routable generation")
    );
}

/// A browser command a live worker takes: the three ids travel separately
/// because the worker echoes the request and treats the other two as opaque.
#[tokio::test]
async fn a_browser_command_reaches_the_worker_with_its_correlation_ids() {
    let fixture = WorkersFixture::new("command").await;
    let socket = Arc::new(RecordingSocket::new());
    fixture.connect_worker(WORKER_FP, "generation-a", &socket);

    let outcome = send_browser_command(
        fixture.registry(),
        &browser_fp(),
        "browser-7",
        "viewer-9",
        "request-3",
        a_kill(),
    );
    assert!(outcome.is_admitted(), "{outcome:?}");
    let frames = socket.frames();
    assert_eq!(frames.len(), 1);
    match &frames[0].0 {
        CoordWorkerDownstream::BrowserCommand {
            browser_id,
            viewer_id,
            request_id,
            frame,
            ..
        } => {
            assert_eq!(browser_id, "browser-7");
            assert_eq!(viewer_id, "viewer-9");
            assert_eq!(request_id, "request-3");
            assert!(matches!(frame, ClientControlFrame::Kill { .. }));
        }
        other => panic!("a browser command is the only frame here, got {other:?}"),
    }
}

/// The reap the workers domain owns: a force-closed session reaches the worker as
/// the kill browser command, under the coordinator's own browser id.
#[tokio::test]
async fn the_orphan_reap_reaches_the_worker_as_a_kill_command() {
    let fixture = WorkersFixture::new("reap").await;
    let socket = Arc::new(RecordingSocket::new());
    fixture.connect_worker(WORKER_FP, "generation-a", &socket);

    let outcome = reap_orphan_pty(fixture.registry(), &browser_fp(), &session_id());
    assert!(outcome.is_admitted(), "{outcome:?}");
    let frames = socket.frames();
    assert_eq!(frames.len(), 1);
    match &frames[0].0 {
        CoordWorkerDownstream::BrowserCommand {
            browser_id,
            viewer_id,
            frame,
            ..
        } => {
            assert_eq!(browser_id, "coord-reap");
            assert_eq!(viewer_id, "coord-reap");
            assert!(matches!(frame, ClientControlFrame::Kill { .. }));
        }
        other => panic!("a reap is a browser command, got {other:?}"),
    }
}

/// A reap for a worker that is offline is refused, and the session is not lost:
/// the next snapshot re-offers it, because the durable effective snapshot has
/// already omitted it and a failed kill cannot resurrect a route.
#[tokio::test]
async fn a_reap_for_an_offline_worker_is_refused_rather_than_lost() {
    let fixture = WorkersFixture::new("reap-offline").await;
    let outcome = reap_orphan_pty(fixture.registry(), &browser_fp(), &session_id());
    assert!(matches!(
        outcome,
        SendOutcome::Refused(SendRefusal::NoRoutableGeneration { .. })
    ));
}

/// A respawn frames the PTY at the size its viewers are already showing, and at
/// the conventional default when nothing is watching.
#[tokio::test]
async fn a_respawn_uses_the_effective_geometry_of_its_viewers() {
    let fixture = WorkersFixture::new("respawn").await;
    fixture.enroll_worker(WORKER_FP, "build-box", 1_000).await;
    fixture.enroll_session(WORKER_FP, SESSION_ID).await;
    let socket = Arc::new(RecordingSocket::new());
    let handle = fixture.connect_worker(WORKER_FP, "generation-a", &socket);
    let views = WatchedGeometry(120, 40);

    let report = respawn_missing_for_worker(
        &fixture.database,
        fixture.registry(),
        &views,
        &WriteGate::new(),
        &handle,
    )
    .await;
    assert_eq!(report.dispatched, 1, "{report:?}");
    assert!(!report.deferred);
    let frames = socket.frames();
    match &frames[0].0 {
        CoordWorkerDownstream::BrowserCommand {
            browser_id,
            viewer_id,
            frame,
            ..
        } => {
            assert_eq!(browser_id, "coord");
            assert_eq!(viewer_id, "coord:respawn");
            match frame {
                ClientControlFrame::RespawnIfMissing {
                    cols, rows, cwd, ..
                } => {
                    assert_eq!((*cols, *rows), (120, 40), "the viewers' geometry");
                    assert_eq!(cwd, "/tmp");
                }
                other => panic!("a respawn is its own frame, got {other:?}"),
            }
        }
        other => panic!("a respawn is a browser command, got {other:?}"),
    }
}

/// With no viewer the PTY is framed at the conventional default, because
/// respawning at a size while views exist makes every attached TUI redraw twice.
#[tokio::test]
async fn a_respawn_with_nothing_watching_uses_the_unwatched_default() {
    let fixture = WorkersFixture::new("respawn-unwatched").await;
    fixture.enroll_worker(WORKER_FP, "build-box", 1_000).await;
    fixture.enroll_session(WORKER_FP, SESSION_ID).await;
    let socket = Arc::new(RecordingSocket::new());
    let handle = fixture.connect_worker(WORKER_FP, "generation-a", &socket);

    let report = respawn_missing_for_worker(
        &fixture.database,
        fixture.registry(),
        &Unwatched,
        &WriteGate::new(),
        &handle,
    )
    .await;
    assert_eq!(report.dispatched, 1, "{report:?}");
    let frames = socket.frames();
    match &frames[0].0 {
        CoordWorkerDownstream::BrowserCommand {
            frame: ClientControlFrame::RespawnIfMissing { cols, rows, .. },
            ..
        } => assert_eq!(
            (*cols, *rows),
            (RESPAWN_UNWATCHED_COLS, RESPAWN_UNWATCHED_ROWS)
        ),
        other => panic!("a respawn is a browser command, got {other:?}"),
    }
}

/// A keeper update holds the exclusive drain, and a respawn waits for it rather
/// than recreating a channel while the coordinator is proving the keeper empty.
#[tokio::test]
async fn a_respawn_is_deferred_while_a_keeper_update_holds_the_gate() {
    let fixture = WorkersFixture::new("respawn-gated").await;
    fixture.enroll_worker(WORKER_FP, "build-box", 1_000).await;
    fixture.enroll_session(WORKER_FP, SESSION_ID).await;
    let socket = Arc::new(RecordingSocket::new());
    let handle = fixture.connect_worker(WORKER_FP, "generation-a", &socket);
    let gate = WriteGate::new();
    let drain = gate
        .acquire_exclusive()
        .expect("an exclusive drain this test owns");

    let report = respawn_missing_for_worker(
        &fixture.database,
        fixture.registry(),
        &Unwatched,
        &gate,
        &handle,
    )
    .await;
    assert!(report.deferred, "{report:?}");
    assert_eq!(report.dispatched, 0);
    assert_eq!(
        socket.count(),
        0,
        "no channel was recreated under the drain"
    );

    drop(drain);
    let report = respawn_missing_for_worker(
        &fixture.database,
        fixture.registry(),
        &Unwatched,
        &gate,
        &handle,
    )
    .await;
    assert_eq!(
        report.dispatched, 1,
        "the pass runs once the drain is released"
    );
}

/// A respawn never writes through a generation that stopped being current.
#[tokio::test]
async fn a_respawn_will_not_write_through_a_fenced_generation() {
    let fixture = WorkersFixture::new("respawn-fenced").await;
    fixture.enroll_worker(WORKER_FP, "build-box", 1_000).await;
    fixture.enroll_session(WORKER_FP, SESSION_ID).await;
    let socket = Arc::new(RecordingSocket::new());
    let handle = fixture.connect_worker(WORKER_FP, "generation-a", &socket);
    fixture.registry().fence(&browser_fp());

    let report = respawn_missing_for_worker(
        &fixture.database,
        fixture.registry(),
        &Unwatched,
        &WriteGate::new(),
        &handle,
    )
    .await;
    assert_eq!(report.dispatched, 0);
    assert_eq!(socket.count(), 0);
    assert!(
        !send_frame(
            fixture.registry(),
            &browser_fp(),
            CoordWorkerDownstream::Ping {
                ts: 1,
                trace_id: None
            }
        )
        .is_admitted(),
        "a fenced generation is not a send target either"
    );
}

fn refusal_text(outcome: &SendOutcome) -> String {
    match outcome {
        SendOutcome::Admitted { .. } => "admitted".to_owned(),
        SendOutcome::Refused(refusal) => refusal.to_string(),
    }
}

/// A view hub with one viewer at a known size.
struct WatchedGeometry(u32, u32);

impl roost_coord::coord_core::TerminalViewLifecycle for WatchedGeometry {
    fn notify_worker_retired(&self, _worker_fp: &WorkerFp, _session_ids: &[SessionId]) {}

    fn effective_geometry(&self, _session_id: &SessionId) -> Option<TerminalGeometry> {
        Some(TerminalGeometry {
            cols: self.0,
            rows: self.1,
        })
    }
}

/// A view hub nobody is watching through.
struct Unwatched;

impl roost_coord::coord_core::TerminalViewLifecycle for Unwatched {
    fn notify_worker_retired(&self, _worker_fp: &WorkerFp, _session_ids: &[SessionId]) {}

    fn effective_geometry(&self, _session_id: &SessionId) -> Option<TerminalGeometry> {
        None
    }
}
