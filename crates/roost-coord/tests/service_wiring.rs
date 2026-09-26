// The eight arms of the ONE `CoordinatorService` impl that are wired to a
// domain handler: five worker methods and three scrollback methods.
//
// Wiring an arm is a delegation with a refusal in front of it. The delegation
// is what the workers and scrollback tests cannot see -- they call a handler
// directly -- so the first three tests here drive the arms and assert the
// handler's own answers come back. The refusals are the authorization-bypass
// guard: an arm that treated a request with no `Caller` as an anonymous one
// would turn the auth interceptor not being mounted into an unauthenticated
// success, and the refusal each wired arm gives instead names the method.
//
// `unwrap`/`expect` are denied outside `#[cfg(test)]`, and an integration test
// is its own crate rather than a module of one, so the exemption is stated here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode, HasMessageView, RequestContext, ServiceRequest};
use roost_coord::auth::principal::Principal;
use roost_coord::coord_core::{Caller, CoordCore, ListenerTrust};
use roost_coord::rpc::service::CoordinatorServiceImpl;
use roost_coord::services::CoordServices;
use roost_proto::buffa::Message;
use roost_proto::buffa::bytes::Bytes;
use roost_proto::roost::v1::CoordinatorService;

/// The account device an operator acts as, and the machine a worker speaks as.
const DEVICE_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const WORKER_FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A coordinator with a migrated database and the one service implementation.
struct ServiceFixture {
    service: CoordinatorServiceImpl,
    root: PathBuf,
}

impl ServiceFixture {
    async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-service-wiring-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated database");
        let config = roost_host::CoordConfig::parse(roost_host::CoordConfigInput {
            db_path: Some(root.join("coord.db")),
            authorized_keys_path: Some(root.join("authorized_keys")),
            log_dir: Some(root.join("logs")),
            ..Default::default()
        })
        .expect("a coordinator config");
        let core = CoordCore::new(Arc::new(CoordServices::new(database)));
        Self {
            service: CoordinatorServiceImpl::new(
                core,
                config,
                "epoch-1".to_owned(),
                0,
                "sha".to_owned(),
            ),
            root,
        }
    }

    /// The account device an operator acts as.
    fn device_caller(&self) -> Caller {
        caller_of(Principal::AccountDevice {
            fingerprint: DEVICE_FP.to_owned(),
            label: "test device".to_owned(),
            account_id: "acct_self_hosted".to_owned(),
        })
    }

    /// A worker, who may not read the fleet view.
    fn worker_caller(&self) -> Caller {
        caller_of(Principal::Worker {
            fingerprint: WORKER_FP.to_owned(),
            label: "test worker".to_owned(),
        })
    }
}

impl Drop for ServiceFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn caller_of(principal: Principal) -> Caller {
    Caller {
        principal,
        tab_id: None,
        remote_address: None,
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}

/// A context carrying the caller the auth interceptor would have stored.
fn context_with(caller: Caller) -> RequestContext {
    let mut context = RequestContext::default();
    context.extensions_mut().insert(caller);
    context
}

/// Bind `$name` to the `ServiceRequest` a generated dispatcher hands an arm,
/// decoded from a body the caller names.
///
/// The request borrows the view and the view borrows the body, so all three are
/// declared in one scope and dropped in the order that keeps both borrows
/// alive; a generated dispatcher owns the body for the length of the call, and
/// so does this.
macro_rules! service_request {
    ($name:ident, $request:ty) => {
        service_request!($name, $request, <$request>::default())
    };
    ($name:ident, $request:ty, $message:expr) => {
        let body = Bytes::from($message.encode_to_vec());
        let view = <$request>::decode_view(&body).expect("the request decodes");
        let $name = ServiceRequest::<$request>::from_parts(&view, &body);
    };
}

/// The refusal a wired method gives for a request with no caller on it: the
/// named `Unimplemented` it would give for an absent domain.
///
/// Naming the method is the point -- a refusal that does not say which method it
/// is cannot be told apart from the sixteen methods v2 never answered -- and
/// `Unimplemented` is what proves no handler ran: every handler here answers a
/// missing caller with `PermissionDenied` or a domain error, and none of them
/// answers with a refusal that names the method.
fn assert_named_refusal(error: ConnectError, method: &str) {
    assert_eq!(error.code, ErrorCode::Unimplemented, "{method}");
    let message = error.message.clone().unwrap_or_default();
    assert!(
        message.starts_with(&format!("{method}:")),
        "{method} refused without naming itself: {message}"
    );
}

/// An operator renames a machine that is not there, and the answer is the
/// handler's own: a rename reads the row and refuses `NotFound`, which no arm
/// in `service_impl.rs` knows how to produce.
#[tokio::test]
async fn a_wired_arm_answers_with_its_handler_s_refusal() {
    let fixture = ServiceFixture::new("reachable").await;
    service_request!(
        request,
        roost_proto::WorkersRenameRequest,
        roost_proto::WorkersRenameRequest {
            fp: WORKER_FP.to_owned(),
            label: "ghost".to_owned(),
            ..Default::default()
        }
    );

    let Err(error) = fixture
        .service
        .workers_rename(context_with(fixture.device_caller()), request)
        .await
    else {
        panic!("there is no such machine");
    };

    assert_eq!(error.code, ErrorCode::NotFound);
}

/// The caller reaches the handler rather than being decided by the arm: only the
/// workers domain knows that a worker may not read the fleet view, and its gate
/// answers `Unauthenticated` where a wired arm's own refusal says
/// `Unimplemented` -- so an arm that invented a caller answers with neither.
#[tokio::test]
async fn a_wired_arm_passes_the_caller_to_its_handler() {
    let fixture = ServiceFixture::new("principal").await;
    service_request!(request, roost_proto::WorkersListRequest);

    let Err(error) = fixture
        .service
        .workers_list(context_with(fixture.worker_caller()), request)
        .await
    else {
        panic!("a worker may not read the fleet view");
    };

    assert_eq!(error.code, ErrorCode::Unauthenticated);
}

/// The success path is the handlers', not the funnel's: an account device reads
/// an empty fleet, where the funnel answered `Unimplemented`.
#[tokio::test]
async fn a_wired_arm_answers_an_authenticated_operator() {
    let fixture = ServiceFixture::new("reachable-ok").await;
    service_request!(request, roost_proto::WorkersListRequest);

    let reply = fixture
        .service
        .workers_list(context_with(fixture.device_caller()), request)
        .await;

    assert!(reply.is_ok(), "an account device reads the fleet view");
}

#[tokio::test]
async fn the_five_worker_arms_refuse_when_no_caller_is_on_the_request() {
    let fixture = ServiceFixture::new("workers-refusal").await;

    service_request!(list, roost_proto::WorkersListRequest);
    let Err(error) = fixture
        .service
        .workers_list(RequestContext::default(), list)
        .await
    else {
        panic!("WorkersList must refuse without a caller");
    };
    assert_named_refusal(error, "WorkersList");

    service_request!(register, roost_proto::WorkersRegisterRequest);
    let Err(error) = fixture
        .service
        .workers_register(RequestContext::default(), register)
        .await
    else {
        panic!("WorkersRegister must refuse without a caller");
    };
    assert_named_refusal(error, "WorkersRegister");

    service_request!(heartbeat, roost_proto::WorkersHeartbeatRequest);
    let Err(error) = fixture
        .service
        .workers_heartbeat(RequestContext::default(), heartbeat)
        .await
    else {
        panic!("WorkersHeartbeat must refuse without a caller");
    };
    assert_named_refusal(error, "WorkersHeartbeat");

    service_request!(rename, roost_proto::WorkersRenameRequest);
    let Err(error) = fixture
        .service
        .workers_rename(RequestContext::default(), rename)
        .await
    else {
        panic!("WorkersRename must refuse without a caller");
    };
    assert_named_refusal(error, "WorkersRename");

    service_request!(delete, roost_proto::WorkersDeleteRequest);
    let Err(error) = fixture
        .service
        .workers_delete(RequestContext::default(), delete)
        .await
    else {
        panic!("WorkersDelete must refuse without a caller");
    };
    assert_named_refusal(error, "WorkersDelete");
}

#[tokio::test]
async fn the_three_scrollback_arms_refuse_when_no_caller_is_on_the_request() {
    let fixture = ServiceFixture::new("scrollback-refusal").await;

    service_request!(cells, roost_proto::SessionsGetScrollbackCellsRequest);
    let Err(error) = fixture
        .service
        .sessions_get_scrollback_cells(RequestContext::default(), cells)
        .await
    else {
        panic!("SessionsGetScrollbackCells must refuse without a caller");
    };
    assert_named_refusal(error, "SessionsGetScrollbackCells");

    service_request!(search, roost_proto::SessionsSearchScrollbackRequest);
    let Err(error) = fixture
        .service
        .sessions_search_scrollback(RequestContext::default(), search)
        .await
    else {
        panic!("SessionsSearchScrollback must refuse without a caller");
    };
    assert_named_refusal(error, "SessionsSearchScrollback");

    service_request!(cancel, roost_proto::SessionsCancelScrollbackSearchRequest);
    let Err(error) = fixture
        .service
        .sessions_cancel_scrollback_search(RequestContext::default(), cancel)
        .await
    else {
        panic!("SessionsCancelScrollbackSearch must refuse without a caller");
    };
    assert_named_refusal(error, "SessionsCancelScrollbackSearch");
}
