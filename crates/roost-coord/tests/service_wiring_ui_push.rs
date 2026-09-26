// The seven arms wired with the UI and push domains: the same refusal an
// unwired method gives, and the two proofs that a wired arm really is a
// delegation -- the caller the arm resolved is the one the handler decides on,
// and the handler's own answer reaches the caller through it.
//
// An arm that invented a caller, or dropped one, would satisfy "the arm does
// not fail" while turning the auth interceptor not being mounted into either an
// authorization bypass or a service that answers machines as browsers.
mod service_wiring_support;

use connectrpc::{ErrorCode, RequestContext};
use roost_proto::roost::v1::CoordinatorService;
use service_wiring_support::{ServiceFixture, assert_named_refusal, context_with, service_request};

#[tokio::test]
async fn the_four_ui_arms_refuse_when_no_caller_is_on_the_request() {
    let fixture = ServiceFixture::new("ui-refusal").await;

    service_request!(report, roost_proto::UiReportStateRequest);
    let Err(error) = fixture
        .service
        .ui_report_state(RequestContext::default(), report)
        .await
    else {
        panic!("UiReportState must refuse without a caller");
    };
    assert_named_refusal(error, "UiReportState");

    service_request!(list, roost_proto::UiListStatesRequest);
    let Err(error) = fixture
        .service
        .ui_list_states(RequestContext::default(), list)
        .await
    else {
        panic!("UiListStates must refuse without a caller");
    };
    assert_named_refusal(error, "UiListStates");

    service_request!(dispatch, roost_proto::UiDispatchRequest);
    let Err(error) = fixture
        .service
        .ui_dispatch(RequestContext::default(), dispatch)
        .await
    else {
        panic!("UiDispatch must refuse without a caller");
    };
    assert_named_refusal(error, "UiDispatch");

    service_request!(apply, roost_proto::UiApplyLayoutRequest);
    let Err(error) = fixture
        .service
        .ui_apply_layout(RequestContext::default(), apply)
        .await
    else {
        panic!("UiApplyLayout must refuse without a caller");
    };
    assert_named_refusal(error, "UiApplyLayout");
}

#[tokio::test]
async fn the_three_push_arms_refuse_when_no_caller_is_on_the_request() {
    let fixture = ServiceFixture::new("push-refusal").await;

    service_request!(config, roost_proto::PushGetConfigRequest);
    let Err(error) = fixture
        .service
        .push_get_config(RequestContext::default(), config)
        .await
    else {
        panic!("PushGetConfig must refuse without a caller");
    };
    assert_named_refusal(error, "PushGetConfig");

    service_request!(subscribe, roost_proto::PushSubscribeRequest);
    let Err(error) = fixture
        .service
        .push_subscribe(RequestContext::default(), subscribe)
        .await
    else {
        panic!("PushSubscribe must refuse without a caller");
    };
    assert_named_refusal(error, "PushSubscribe");

    service_request!(unsubscribe, roost_proto::PushUnsubscribeRequest);
    let Err(error) = fixture
        .service
        .push_unsubscribe(RequestContext::default(), unsubscribe)
        .await
    else {
        panic!("PushUnsubscribe must refuse without a caller");
    };
    assert_named_refusal(error, "PushUnsubscribe");
}

/// The caller an arm resolves is the one the handler decides on: the same arm
/// serves a fenced device and refuses a machine, and the refusal it gives a
/// machine is the domain's, not the arm's.
///
/// Both halves matter. An arm that invented a device caller would answer the
/// worker with a served list, and an arm that dropped the caller would refuse
/// both with the same named `Unimplemented` it gives an absent domain.
#[tokio::test]
async fn a_ui_arm_hands_the_caller_it_resolved_to_the_handler_that_decides() {
    let fixture = ServiceFixture::new("ui-delegation").await;

    service_request!(list_for_device, roost_proto::UiListStatesRequest);
    let listed = fixture
        .service
        .ui_list_states(context_with(fixture.tab_device_caller()), list_for_device)
        .await;
    assert!(
        listed.is_ok(),
        "a fenced device reaches the handler and is served; the arm's own answer is a refusal"
    );

    service_request!(list_for_worker, roost_proto::UiListStatesRequest);
    let Err(error) = fixture
        .service
        .ui_list_states(context_with(fixture.worker_caller()), list_for_worker)
        .await
    else {
        panic!("a worker is not a browser, and only the handler knows that");
    };
    assert_eq!(error.code, ErrorCode::Unauthenticated);
    assert!(
        !error
            .message
            .as_deref()
            .unwrap_or_default()
            .starts_with("UiListStates:"),
        "the arm's own refusal would name the method; the handler's does not"
    );
}

/// A wired push arm reaches the push handler, which knows a core built without
/// a tenancy scope is a wiring fault -- an answer no arm in `service_impl.rs`
/// can produce.
#[tokio::test]
async fn a_push_arm_hands_the_caller_it_resolved_to_the_push_handler() {
    let fixture = ServiceFixture::new("push-delegation").await;

    service_request!(config, roost_proto::PushGetConfigRequest);
    let Err(error) = fixture
        .service
        .push_get_config(context_with(fixture.device_caller()), config)
        .await
    else {
        panic!("this core has no push runtime, and the handler is what says so");
    };
    assert_eq!(error.code, ErrorCode::Internal);
    assert!(
        error
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("push runtime is not installed"),
        "the push handler's own wiring fault: {}",
        error.message.as_deref().unwrap_or_default()
    );
}
