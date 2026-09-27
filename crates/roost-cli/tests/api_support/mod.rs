//! A coordinator in this test process, serving the generated `CoordinatorService`
//! methods the `roost api` behaviour tests drive. Owned by the L5 slice.
//!
//! WHY A SERVER RATHER THAN A STUBBED CLIENT. The question these tests exist to
//! answer is "does the verb reach a real generated method", and a stubbed client
//! answers that by construction — the stub is the thing the test wrote. Here the
//! request leaves the process over HTTP through `CoordinatorServiceClient`, is
//! routed by the spec constant the generator emitted, and comes back decoded
//! into that spec's own response type. A verb wired to a hand-typed path fails
//! this and passes nothing.
//!
//! THE METHODS ARE MOUNTED FROM THEIR GENERATED SPECS, NOT THEIR NAMES. Each
//! route is registered under `spec.service()` / `spec.method()` with the spec
//! itself attached, which is what the generated `CoordinatorServiceExt` does for
//! the whole service. Typing a method name as a string here would put a second
//! copy of a generator-owned name into the test suite, which is the defect the
//! product code exists to avoid.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use connectrpc::Spec;
use connectrpc::handler::handler_fn;
use connectrpc::service::ConnectRpcService;
use roost_proto::buffa::{Inline, MessageField};
use roost_proto::{
    AgentStatusGetRequest, AgentStatusGetResponse, AgentStatusListRequest,
    AgentStatusListResponse, AgentStatusView, AgentPromptWaitOutcome, AgentStatusWaitRequest,
    AgentStatusWaitResponse, SessionsListRequest, SessionsListResponse, Worker, WorkersListRequest,
    WorkersListResponse,
};

/// The status a session's agent is in, fenced to a named occupant.
pub const SESSION_ID: &str = "session-1";

/// What this process's coordinator answers.
///
/// Canned values rather than closures: a closure here would let a test script
/// the coordinator's behaviour, and the point is that the coordinator's *shape*
/// is real even when its contents are not.
#[derive(Debug, Default, Clone)]
pub struct Fixture {
    pub workers: Vec<Worker>,
    pub routable_fps: Vec<String>,
    pub sessions: Vec<roost_proto::Session>,
    /// What `AgentStatusGet` answers on its first read, and on its second. The
    /// second is what `agent-wait` reports, so a test can move the agent
    /// between the two reads.
    pub first_status: Option<AgentStatusView>,
    pub second_status: Option<AgentStatusView>,
    pub wait_outcome: Option<AgentPromptWaitOutcome>,
    reads: Arc<AtomicUsize>,
}

impl Fixture {
    /// A status an agent is in, fenced to a named occupant.
    #[must_use]
    pub fn status(state: &str, revision: u64) -> AgentStatusView {
        AgentStatusView {
            session_id: SESSION_ID.to_string(),
            agent_id: "omp".to_string(),
            state: state.to_string(),
            message: None,
            revision,
            completed_revision: revision,
            updated_at: 1_700_000_000.0,
            active: true,
            status_epoch: Some("11111111-1111-4111-8111-111111111111".to_string()),
            occupant_id: Some("22222222-2222-4222-8222-222222222222".to_string()),
            source: Some("integration".to_string()),
            promptable: true,
            ..Default::default()
        }
    }
}

/// Bind a loopback listener, mount the fixture's methods, and serve until the
/// returned handle is dropped. The string is the origin to point a client at.
pub async fn serve(fixture: Fixture) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is available in a test process");
    let origin = format!("http://{}", listener.local_addr().expect("a bound address"));
    let app = axum::Router::new().fallback_service(ConnectRpcService::new(router(fixture)));
    let handle = tokio::spawn(async move {
        // The server only ends when the test aborts this task, so there is no
        // error to report; `drop` says that outright where `let _ =` reads as
        // a discarded value the author meant to look at.
        drop(axum::serve(listener, app).await);
    });
    (origin, handle)
}

fn router(fixture: Fixture) -> connectrpc::Router {
    let workers = fixture.clone();
    let sessions = fixture.clone();
    let status = fixture.clone();
    let listed = fixture.clone();
    let wait = fixture;
    let mut router = connectrpc::Router::new();
    router = mount(
        router,
        roost_proto::COORDINATOR_SERVICE_WORKERS_LIST_SPEC,
        handler_fn(move |_ctx, _request: WorkersListRequest| {
                let workers = workers.clone();
                async move {
                    Ok(connectrpc::Response::new(WorkersListResponse {
                        workers: workers.workers.clone(),
                        routable_fps: workers.routable_fps.clone(),
                        ..Default::default()
                    }))
                }
            }),
    );
    router = mount(
        router,
        roost_proto::COORDINATOR_SERVICE_SESSIONS_LIST_SPEC,
        handler_fn(move |_ctx, _request: SessionsListRequest| {
                let sessions = sessions.clone();
                async move {
                    Ok(connectrpc::Response::new(SessionsListResponse {
                        sessions: sessions.sessions.clone(),
                        ..Default::default()
                    }))
                }
            }),
    );
    router = mount(
        router,
        roost_proto::COORDINATOR_SERVICE_AGENT_STATUS_GET_SPEC,
        handler_fn(move |_ctx, _request: AgentStatusGetRequest| {
                let status = status.clone();
                let first = status.reads.fetch_add(1, Ordering::SeqCst) == 0;
                async move {
                    Ok(connectrpc::Response::new(AgentStatusGetResponse {
                        status: field(if first {
                            status.first_status.clone()
                        } else {
                            status.second_status.clone().or(status.first_status.clone())
                        }),
                        ..Default::default()
                    }))
                }
            }),
    );
    router = mount(
        router,
        roost_proto::COORDINATOR_SERVICE_AGENT_STATUS_LIST_SPEC,
        handler_fn(move |_ctx, _request: AgentStatusListRequest| {
                let listed = listed.clone();
                async move {
                    Ok(connectrpc::Response::new(AgentStatusListResponse {
                        statuses: listed.first_status.clone().into_iter().collect(),
                        ..Default::default()
                    }))
                }
            }),
    );
    router = mount(
        router,
        roost_proto::COORDINATOR_SERVICE_AGENT_STATUS_WAIT_SPEC,
        handler_fn(move |_ctx, _request: AgentStatusWaitRequest| {
                let wait = wait.clone();
                async move {
                    Ok(connectrpc::Response::new(AgentStatusWaitResponse {
                        outcome: outcome_name(wait.wait_outcome),
                        ..Default::default()
                    }))
                }
            }),
        );
    router
}

/// Register one generated method, under the service and method its own spec
/// names, and carry that spec into the router's method table.
fn mount<H, Req, Res>(
    router: connectrpc::Router,
    spec: Spec,
    handler: H,
) -> connectrpc::Router
where
    H: connectrpc::Handler<Req, Res>,
    Req: roost_proto::buffa::message::Message
        + serde::de::DeserializeOwned
        + serde::Serialize
        + Send
        + 'static,
    Res: roost_proto::buffa::message::Message + serde::Serialize + Send + 'static,
{
    router
        .route(spec.service(), spec.method(), handler)
        .with_spec(spec)
}

/// The published wait outcome name, matched as the generated variant rather
/// than as the wire number.
fn outcome_name(outcome: Option<AgentPromptWaitOutcome>) -> String {
    match outcome {
        Some(AgentPromptWaitOutcome::Matched) => "matched",
        Some(AgentPromptWaitOutcome::TimedOut) => "timed_out",
        Some(AgentPromptWaitOutcome::OccupantChanged) => "occupant_changed",
        Some(AgentPromptWaitOutcome::SessionClosed) => "session_closed",
        Some(AgentPromptWaitOutcome::PromptStalled) => "prompt_stalled",
        // The product refuses this one (`wait_outcome_name` maps UNSPECIFIED to
        // "a wait outcome this build does not know"), so the fake must never
        // publish it. It renders to a name no real outcome collides with,
        // which makes a coordinator that did publish it fail the assertion
        // instead of reading as a timeout.
        Some(AgentPromptWaitOutcome::AGENT_PROMPT_WAIT_OUTCOME_UNSPECIFIED) => "unspecified",
        None => "timed_out",
    }
    .to_string()
}

/// A present-or-absent message field, which is how the contract models one.
fn field(view: Option<AgentStatusView>) -> MessageField<AgentStatusView, Inline<AgentStatusView>> {
    view.map_or_else(MessageField::none, MessageField::some)
}
