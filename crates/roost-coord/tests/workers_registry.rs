// The generation fence and the routable set: which generation may carry a
// frame, when it stops being able to, and what a retirement releases.
//
// Every test here drives `coord_core::worker_handle` directly rather than
// through a handler, because the fence is a property of the REGISTRY and not of
// any method: a socket that arrives, a socket that is replaced, a credential
// that is consumed and a socket that goes away are four transport events, and
// only the last one is reachable from a Connect method.

mod workers_support;

use std::sync::{Arc, Mutex};

use roost_coord::coord_core::seams::{LiveChannel, TerminalViewLifecycle, WorkerRouteIndex};
use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_coord::workers::registry::{
    claim_generation, list_routable_fps, mark_generation_ready, publish_routable,
};
use roost_coord::workers::rpc::handle_workers_delete;
use roost_coord::workers::send::{SendOutcome, SendRefusal, send_frame, send_frame_through};
use roost_protocol::viewport::TerminalGeometry;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use roost_protocol::wire::{ChannelId, SessionId, WorkerFp};

use workers_support::{RecordingSocket, SESSION_ID, WORKER_FP, WorkersFixture, device_caller};

fn browser_fp() -> WorkerFp {
    WorkerFp::try_from(WORKER_FP).expect("a fingerprint the brand accepts")
}

fn a_browser_command() -> CoordWorkerDownstream {
    CoordWorkerDownstream::BrowserCommand {
        browser_id: "browser-1".to_owned(),
        viewer_id: "viewer-1".to_owned(),
        request_id: "request-1".to_owned(),
        frame: ClientControlFrame::Kill {
            session_id: SessionId::try_from(SESSION_ID).expect("a session id"),
            trace_id: None,
        },
        trace_id: None,
    }
}

/// A second generation takes the fingerprint, and the first one is dead: a
/// handle a caller kept across the swap refuses rather than delivering.
#[tokio::test]
async fn a_second_generation_fences_the_first() {
    let fixture = WorkersFixture::new("fence").await;
    fixture.enroll_worker(WORKER_FP, "build-box", 1_000).await;
    let first_socket = Arc::new(RecordingSocket::new());
    let first = fixture.connect_worker(WORKER_FP, "generation-a", &first_socket);

    // A reconnect arrives while the old socket is still open.
    let second_socket = Arc::new(RecordingSocket::new());
    let second = fixture.connect_worker(WORKER_FP, "generation-b", &second_socket);
    assert!(first.is_revoked(), "the replaced generation is fenced");
    assert!(second.is_routable(), "the new generation is routable");

    // The handle a handler resolved before the swap cannot write.
    let through_stale = send_frame_through(&fixture.registry(), &first, a_browser_command());
    assert!(
        matches!(
            through_stale,
            SendOutcome::Refused(SendRefusal::SupersededGeneration { .. })
        ),
        "a superseded generation is refused, got {through_stale:?}"
    );
    assert_eq!(
        first_socket.count(),
        0,
        "the stale socket was handed nothing"
    );
    assert_eq!(
        first.send(a_browser_command()),
        0,
        "a retained handle stops reaching the transport it was given"
    );
    assert_eq!(first_socket.count(), 0, "and it stays that way");

    // The current generation takes the frame.
    let through_current = send_frame(&fixture.registry(), &browser_fp(), a_browser_command());
    assert!(through_current.is_admitted(), "{through_current:?}");
    assert_eq!(second_socket.count(), 1);
    assert_eq!(first_socket.count(), 0);
}

/// A retired generation is dead to a caller that kept the handle, and the
/// worker leaves the routable set.
#[tokio::test]
async fn a_retired_generation_is_dead_to_everyone() {
    let fixture = WorkersFixture::new("retire").await;
    let socket = Arc::new(RecordingSocket::new());
    let handle = fixture.connect_worker(WORKER_FP, "generation-a", &socket);
    assert_eq!(list_routable_fps(fixture.registry()), vec![browser_fp()]);

    fixture.registry().retire(&browser_fp());
    publish_routable(&fixture.core.services.buses, fixture.registry());

    assert!(handle.is_revoked());
    assert!(
        list_routable_fps(fixture.registry()).is_empty(),
        "a retired worker is not routable"
    );
    let refused = send_frame(&fixture.registry(), &browser_fp(), a_browser_command());
    assert_eq!(
        refused,
        SendOutcome::Refused(SendRefusal::NoRoutableGeneration {
            worker_fp: browser_fp()
        })
    );
    assert_eq!(socket.count(), 0, "nothing was written to a retired socket");
}

/// A fenced credential is not routable, which is what keeps a revoked
/// generation out of the fleet view's online set while its socket is still up.
#[tokio::test]
async fn a_fenced_credential_leaves_the_routable_set() {
    let fixture = WorkersFixture::new("fenced").await;
    let socket = Arc::new(RecordingSocket::new());
    let handle: Arc<WorkerHandle> = fixture.connect_worker(WORKER_FP, "generation-a", &socket);
    assert_eq!(list_routable_fps(fixture.registry()), vec![browser_fp()]);

    let fenced = fixture.registry().fence(&browser_fp());
    assert_eq!(
        fenced.map(|handle| handle.connection_generation.clone()),
        Some(handle.connection_generation.clone()),
        "the fence names the generation it revoked"
    );
    assert!(
        list_routable_fps(fixture.registry()).is_empty(),
        "generation {} is fenced and must not be offered to a browser",
        handle.connection_generation
    );
    let refused = send_frame(&fixture.registry(), &browser_fp(), a_browser_command());
    assert!(
        matches!(
            refused,
            SendOutcome::Refused(SendRefusal::NoRoutableGeneration { .. })
        ),
        "{refused:?}"
    );
    assert_eq!(socket.count(), 0, "a fenced generation is handed nothing");
}

/// A generation that has not crossed its snapshot barrier is not routable: a
/// hello claims the fingerprint, and only the exact snapshot makes it usable.
#[tokio::test]
async fn a_claimed_generation_is_not_routable_until_its_snapshot_commits() {
    let fixture = WorkersFixture::new("claimed").await;
    let socket = Arc::new(RecordingSocket::new());
    let handle = Arc::new(WorkerHandle::new(
        browser_fp(),
        Some("epoch-1".to_owned()),
        "generation-a".to_owned(),
        std::collections::BTreeSet::new(),
        socket.sender(),
    ));
    claim_generation(
        &fixture.core.services.buses,
        fixture.registry(),
        Arc::clone(&handle),
    );
    assert!(!handle.is_ready(), "a claim is not readiness");
    assert!(list_routable_fps(fixture.registry()).is_empty());
    assert!(!send_frame(&fixture.registry(), &browser_fp(), a_browser_command()).is_admitted());

    assert!(mark_generation_ready(
        &fixture.core.services.buses,
        fixture.registry(),
        &handle
    ));
    assert_eq!(list_routable_fps(fixture.registry()), vec![browser_fp()]);
    assert!(send_frame(&fixture.registry(), &browser_fp(), a_browser_command()).is_admitted());
    assert!(
        !mark_generation_ready(&fixture.core.services.buses, fixture.registry(), &handle),
        "a duplicate snapshot does not republish the worker"
    );
}

/// A delete tells the view hub about every session that lost a route, whether it
/// had a durable row or only a live one.
#[tokio::test]
async fn a_retired_route_releases_the_session_it_carried() {
    let mut fixture = WorkersFixture::new("release").await;
    fixture.enroll_worker(WORKER_FP, "build-box", 1_000).await;
    fixture.enroll_session(WORKER_FP, SESSION_ID).await;
    let socket = Arc::new(RecordingSocket::new());
    fixture.connect_worker(WORKER_FP, "generation-a", &socket);
    let routes = Arc::new(RecordingRoutes::default());
    let views = Arc::new(RecordingViews::default());
    let erased_routes: Arc<dyn WorkerRouteIndex> = routes.clone();
    let erased_views: Arc<dyn TerminalViewLifecycle> = views.clone();
    fixture.attach_terminal_seams(erased_routes, erased_views);

    handle_workers_delete(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkersDeleteRequest {
            fp: WORKER_FP.to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("a delete");

    assert_eq!(
        routes.retired(),
        vec![browser_fp()],
        "the byte hub was told to drop every route of the deleted worker"
    );
    let notified = views.notified();
    assert_eq!(notified.len(), 1, "the view hub was told exactly once");
    assert_eq!(notified[0].0.as_str(), WORKER_FP);
    assert_eq!(
        notified[0].1,
        vec![SessionId::try_from(SESSION_ID).expect("a session id")],
        "the session that lost its route is named, so its cleanup can be retried"
    );
}

/// The two route cleanups are separate steps, and the view hub failing must not
/// put the retired route back.
#[tokio::test]
async fn a_failing_view_cleanup_does_not_undo_the_route_retirement() {
    let mut fixture = WorkersFixture::new("retire-routes").await;
    fixture.enroll_worker(WORKER_FP, "build-box", 1_000).await;
    fixture.enroll_session(WORKER_FP, SESSION_ID).await;
    let socket = Arc::new(RecordingSocket::new());
    fixture.connect_worker(WORKER_FP, "generation-a", &socket);
    let routes: Arc<dyn WorkerRouteIndex> = Arc::new(RecordingRoutes::default());
    let views: Arc<dyn TerminalViewLifecycle> = Arc::new(FailingViews);
    fixture.attach_terminal_seams(routes, views);

    let deleted = handle_workers_delete(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkersDeleteRequest {
            fp: WORKER_FP.to_owned(),
            ..Default::default()
        },
    )
    .await;
    assert!(
        deleted.is_ok(),
        "a collaborator that throws must not fail an irrevocable delete: {:?}",
        deleted.err()
    );
    assert!(
        deleted.expect("a delete").body.ok,
        "the delete reports success"
    );
}

/// A byte hub that records what it was told to retire.
#[derive(Debug, Default)]
struct RecordingRoutes {
    retired: Mutex<Vec<WorkerFp>>,
}

impl RecordingRoutes {
    fn retired(&self) -> Vec<WorkerFp> {
        self.retired
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl WorkerRouteIndex for RecordingRoutes {
    fn lookup_session_id(&self, worker_fp: &WorkerFp, channel_id: &ChannelId) -> Option<SessionId> {
        let _ = (worker_fp, channel_id);
        None
    }

    fn replace_worker_channel_index(&self, worker_fp: &WorkerFp, live: &[LiveChannel]) {
        self.retired
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(worker_fp.clone());
        let _ = live;
    }

    fn retire_worker_routes(&self, worker_fp: &WorkerFp) -> Vec<SessionId> {
        self.retired
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(worker_fp.clone());
        // A live route for a session with no durable row yet: the common case for
        // a session opened seconds before the machine was removed.
        vec![SessionId::try_from(SESSION_ID).expect("a session id")]
    }
}

/// A view hub that records the retirement.
#[derive(Debug, Default)]
struct RecordingViews {
    notified: Mutex<Vec<(WorkerFp, Vec<SessionId>)>>,
}

impl RecordingViews {
    fn notified(&self) -> Vec<(WorkerFp, Vec<SessionId>)> {
        self.notified
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl TerminalViewLifecycle for RecordingViews {
    fn notify_worker_retired(&self, worker_fp: &WorkerFp, session_ids: &[SessionId]) {
        self.notified
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((worker_fp.clone(), session_ids.to_vec()));
    }

    fn effective_geometry(&self, _session_id: &SessionId) -> Option<TerminalGeometry> {
        None
    }
}

/// A view hub whose cleanup fails, which is the failure a delete must absorb.
#[derive(Debug)]
struct FailingViews;

impl TerminalViewLifecycle for FailingViews {
    fn notify_worker_retired(&self, _worker_fp: &WorkerFp, _session_ids: &[SessionId]) {
        panic!("the view hub is out of memory");
    }

    fn effective_geometry(&self, _session_id: &SessionId) -> Option<TerminalGeometry> {
        None
    }
}
