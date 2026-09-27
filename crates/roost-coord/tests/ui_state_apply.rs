//! The acknowledged layout apply over a real database and the live UI bus:
//! the socket reservation, the target-gone answer, and the two fences that must
//! refuse before anything is reserved.
//!
//! The distinction this file guards is the one in the Sync contract: an
//! applyLayout RESULT settles the layout-apply owner, and the apply itself is
//! addressed to one exact socket generation. A target that is merely listed, or
//! a tab id another device also claims, must never receive one.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod ui_state_fixture;

use std::sync::{Arc, Mutex};

use connectrpc::ErrorCode;
use roost_coord::events::bus_messages::UiBusMsg;
use roost_coord::ui_state::layout_apply::UiLayoutApplyTarget;
use roost_coord::ui_state::rpc::{handle_ui_apply_layout, handle_ui_report_state};
use roost_proto as proto;
use roost_proto::__buffa::oneof::ui_command::Command;
use roost_proto::buffa::MessageField;
use ui_state_fixture::{
    FOREIGN_SESSION_ID, SESSION_ID, UiStateFixture, browser_fingerprint, collect_ui_bus,
    layout_document, report_request,
};

fn apply_request(
    target_fingerprint: &str,
    target_tab_id: &str,
    session_id: &str,
) -> proto::UiApplyLayoutRequest {
    proto::UiApplyLayoutRequest {
        target_tab_id: target_tab_id.to_owned(),
        target_fingerprint: target_fingerprint.to_owned(),
        document: MessageField::some(layout_document(session_id)),
        ..Default::default()
    }
}

#[tokio::test]
async fn an_apply_reserves_the_named_socket_and_resolves_with_its_acknowledgement() {
    let fixture = UiStateFixture::new("apply").await;
    let target = UiLayoutApplyTarget {
        fingerprint: browser_fingerprint('a'),
        tab_id: "tab-1".to_owned(),
        socket_id: "socket-1".to_owned(),
    };
    let _guard = fixture
        .runtime
        .layout_applies()
        .register_target(target.clone())
        .expect("the target socket registers");

    let seen: Arc<Mutex<Vec<UiBusMsg>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let acknowledging = target.clone();
    let runtime = fixture.runtime.clone();
    let subscription = fixture
        .core
        .services
        .buses
        .ui_bus
        .subscribe(move |message: &UiBusMsg| {
            let UiBusMsg::Apply {
                correlation_id,
                command,
                ..
            } = message
            else {
                return;
            };
            if !matches!(command.command, Some(Command::ApplyLayout(_))) {
                return;
            }
            sink.lock()
                .expect("the bus sink lock")
                .push(message.clone());
            runtime.layout_applies().accept_result(
                &acknowledging,
                &proto::UiApplyLayoutResult {
                    correlation_id: correlation_id.clone(),
                    outcome: proto::UiApplyLayoutOutcome::Applied.into(),
                    ..Default::default()
                },
            );
        });

    let response = handle_ui_apply_layout(
        &fixture.core,
        &fixture.caller('a'),
        apply_request(&target.fingerprint, &target.tab_id, SESSION_ID),
    )
    .await
    .expect("an apply against a live socket is admitted");
    drop(subscription);

    assert_eq!(response.body.outcome, proto::UiApplyLayoutOutcome::Applied);
    assert!(
        !response.body.correlation_id.is_empty(),
        "the caller is told which correlation its target answered"
    );
    assert_eq!(response.body.reason, None);
    assert_eq!(fixture.runtime.layout_applies().stats().pending, 0);

    let published = seen.lock().expect("the bus sink lock").clone();
    let UiBusMsg::Apply {
        target_socket_id,
        command,
        ..
    } = published.first().expect("exactly one apply is published")
    else {
        panic!("an apply publishes an apply message");
    };
    assert_eq!(
        target_socket_id, "socket-1",
        "the apply is pinned to the exact socket generation"
    );
    let Some(Command::ApplyLayout(apply)) = command.command.as_ref() else {
        panic!("the published command is an applyLayout");
    };
    assert_eq!(
        apply
            .document
            .as_option()
            .map(|document| document.bindings[0].session_id.clone()),
        Some(SESSION_ID.to_owned())
    );
}

#[tokio::test]
async fn an_apply_for_a_reported_but_socketless_tab_is_answered_target_gone() {
    let fixture = UiStateFixture::new("apply-gone").await;
    handle_ui_report_state(
        &fixture.core,
        &fixture.caller('a'),
        report_request("tab-1", "/s/one", None),
    )
    .await
    .expect("the tab reports, so it is listed");

    let (response, messages) = collect_ui_bus(&fixture, async {
        handle_ui_apply_layout(
            &fixture.core,
            &fixture.caller('a'),
            apply_request(&browser_fingerprint('a'), "tab-1", SESSION_ID),
        )
        .await
    })
    .await;
    let response = response.expect("a socketless target is an answer, not an error");
    assert_eq!(
        response.body.outcome,
        proto::UiApplyLayoutOutcome::TargetGone
    );
    assert!(messages.is_empty(), "nothing was published to nobody");
    assert_eq!(
        fixture.runtime.states().retained_count(),
        1,
        "the tab is still listed: it reports, it simply has no live socket"
    );
}

#[tokio::test]
async fn an_apply_naming_an_unpersisted_session_is_refused_before_any_reservation() {
    let fixture = UiStateFixture::new("apply-foreign").await;
    let target = UiLayoutApplyTarget {
        fingerprint: browser_fingerprint('a'),
        tab_id: "tab-1".to_owned(),
        socket_id: "socket-1".to_owned(),
    };
    let _guard = fixture
        .runtime
        .layout_applies()
        .register_target(target.clone())
        .expect("the target socket registers");

    let (result, messages) = collect_ui_bus(&fixture, async {
        handle_ui_apply_layout(
            &fixture.core,
            &fixture.caller('a'),
            apply_request(&target.fingerprint, &target.tab_id, FOREIGN_SESSION_ID),
        )
        .await
    })
    .await;
    assert_eq!(
        result
            .expect_err("an apply naming an unpersisted session is refused")
            .code,
        ErrorCode::NotFound
    );
    assert!(
        messages.is_empty(),
        "a refused apply published nothing: {messages:?}"
    );
    assert_eq!(
        fixture.runtime.layout_applies().stats().pending,
        0,
        "a refused apply reserved nothing"
    );
}

#[tokio::test]
async fn an_apply_without_a_target_fingerprint_or_document_is_refused() {
    let fixture = UiStateFixture::new("apply-required").await;
    for request in [
        proto::UiApplyLayoutRequest {
            target_tab_id: "tab-1".to_owned(),
            target_fingerprint: String::new(),
            document: MessageField::some(layout_document(SESSION_ID)),
            ..Default::default()
        },
        proto::UiApplyLayoutRequest {
            target_tab_id: "tab-1".to_owned(),
            target_fingerprint: browser_fingerprint('a'),
            document: MessageField::none(),
            ..Default::default()
        },
    ] {
        let refused = handle_ui_apply_layout(&fixture.core, &fixture.caller('a'), request)
            .await
            .expect_err("a semantically required field is missing");
        assert_eq!(refused.code, ErrorCode::InvalidArgument);
    }
}

#[tokio::test]
async fn an_apply_refuses_a_request_that_carried_no_tab_id() {
    let fixture = UiStateFixture::new("apply-no-tab").await;
    let target = UiLayoutApplyTarget {
        fingerprint: browser_fingerprint('a'),
        tab_id: "tab-1".to_owned(),
        socket_id: "socket-1".to_owned(),
    };
    let _guard = fixture
        .runtime
        .layout_applies()
        .register_target(target.clone())
        .expect("the target socket registers");

    let refused = handle_ui_apply_layout(
        &fixture.core,
        &fixture.caller_without_tab('a'),
        apply_request(&target.fingerprint, &target.tab_id, SESSION_ID),
    )
    .await
    .expect_err("an apply from a request with no tab id is refused");
    assert_eq!(refused.code, ErrorCode::FailedPrecondition);
    assert!(
        refused
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("tab fence"),
        "the refusal must name the fence it is missing"
    );
    assert_eq!(
        fixture.runtime.layout_applies().stats().pending,
        0,
        "a refused apply reserved nothing"
    );
}
