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
// The allow belongs to THIS compilation unit, not to the test binary that
// happens to declare `mod api_support;`. `clippy.toml`'s
// `allow-unwrap-in-tests` exempts a `#[test]` body; `serve` is an ordinary
// function, so without the declaration below every `expect` in it is an
// `expect_used` error the moment clippy runs -- and which binary pulls the
// fixture in is not this file's to decide.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use connectrpc::Spec;
use connectrpc::handler::handler_fn;
use connectrpc::service::ConnectRpcService;
use roost_proto::{
    PbCellRow, PbCellSpan, SessionsGetScrollbackCellsRequest, SessionsGetScrollbackCellsResponse,
    SessionsListRequest, SessionsListResponse, Worker, WorkersListRequest, WorkersListResponse,
};

/// A session identifier used by the terminal fixtures.
pub const SESSION_ID: &str = "33333333-3333-4333-8333-333333333333";

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
    pub scrollback_rows: Vec<String>,
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
    let scrollback = fixture;
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
        roost_proto::COORDINATOR_SERVICE_SESSIONS_GET_SCROLLBACK_CELLS_SPEC,
        handler_fn(move |_ctx, request: SessionsGetScrollbackCellsRequest| {
            let scrollback = scrollback.clone();
            async move {
                roost_coord::terminal_screen::scrollback_window::require_json_safe_row(
                    request.end_row,
                    "scrollback cells end_row",
                )?;
                let total = scrollback.scrollback_rows.len() as u64;
                let end_row = request.end_row.min(total);
                let start_row = end_row.saturating_sub(u64::from(request.max_rows));
                let rows = (start_row..end_row)
                    .map(|index| PbCellRow {
                        index: index as u32,
                        spans: vec![PbCellSpan {
                            text: scrollback.scrollback_rows[index as usize].clone(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    })
                    .collect();
                Ok(connectrpc::Response::new(
                    SessionsGetScrollbackCellsResponse {
                        rows,
                        cols: 80,
                        scrollback_total: total,
                        start_row,
                        end_row,
                        ..Default::default()
                    },
                ))
            }
        }),
    );
    router
}

/// Register one generated method, under the service and method its own spec
/// names, and carry that spec into the router's method table.
fn mount<H, Req, Res>(router: connectrpc::Router, spec: Spec, handler: H) -> connectrpc::Router
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
