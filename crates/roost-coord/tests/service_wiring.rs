// The arms of the ONE `CoordinatorService` impl that hand a request to a domain
// handler: the delegation, and the refusal a wired method still answers with
// when the request carried no caller at all.
//
// The UI and push arms are asserted in `service_wiring_ui_push.rs`; the fixture
// both files build is `service_wiring_support`, which is the single owner of
// "a coordinator with a migrated database".
mod service_wiring_support;

use connectrpc::{ErrorCode, RequestContext};
use roost_proto::roost::v1::CoordinatorService;
use service_wiring_support::{
    ServiceFixture, WORKER_FP, assert_named_refusal, context_with, service_request,
};

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
