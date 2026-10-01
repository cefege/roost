//! The tab fence on the layout-apply path: which live browser generation an
//! apply may reach, which requests may ask, and what a generation that has
//! died is answered instead of being left on the bus.
//!
//! These are the coordinator's half of `smoke/terminal/ui-layout-apply.spec.ts`
//! ("acknowledged layout apply targets one live browser generation"). The spec
//! proves the same three answers against two real browser tabs; this file pins
//! them where a regression would be a one-line change away -- including the
//! refusal a fix for that spec's first poll must not quietly remove.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod ui_state_fixture;

use std::sync::{Arc, Mutex};

use connectrpc::ErrorCode;
use roost_coord::coord_core::CoordCore;
use roost_coord::events::bus_messages::UiBusMsg;
use roost_coord::sync_ws::feed::ui::{UiViewer, ui_bus_frame};
use roost_coord::ui_state::layout_apply::{
    LayoutApplyOwnerStats, LayoutApplyRequest, UI_LAYOUT_TARGET_GONE_REASON,
    UiLayoutApplyPublication, UiLayoutApplyTarget,
};
use roost_coord::ui_state::rpc::{
    handle_ui_apply_layout, handle_ui_list_states, handle_ui_report_state,
};
use roost_proto as proto;
use roost_proto::__buffa::oneof::ui_command::Command;

use ui_state_fixture::{
    SESSION_ID, UiStateFixture, applied, apply_layout_command, apply_request, browser_fingerprint,
    collect_ui_bus, layout_document, report_request,
};

/// Publish one reserved apply on the live UI bus, as the RPC's publish step does.
fn publish_reserved_apply(core: &CoordCore, publication: &UiLayoutApplyPublication) {
    core.services.buses.ui_bus.publish(UiBusMsg::Apply {
        target_tab_id: publication.target.tab_id.clone(),
        target_socket_id: publication.target.socket_id.clone(),
        correlation_id: publication.correlation_id.clone(),
        command: apply_layout_command(SESSION_ID),
    });
}

#[tokio::test]
async fn a_fenced_apply_reaches_exactly_the_named_live_browser_generation() {
    let fixture = UiStateFixture::new("tab-fence-one-generation").await;
    let named = UiLayoutApplyTarget {
        fingerprint: browser_fingerprint('a'),
        tab_id: "tab-1".to_owned(),
        socket_id: "socket-named".to_owned(),
    };
    let sibling = UiLayoutApplyTarget {
        fingerprint: browser_fingerprint('a'),
        tab_id: "tab-2".to_owned(),
        socket_id: "socket-sibling".to_owned(),
    };
    // Another device claiming the same tab id is the collision the registration
    // key exists to keep apart, so it is live here too.
    let collider = UiLayoutApplyTarget {
        fingerprint: browser_fingerprint('b'),
        tab_id: "tab-1".to_owned(),
        socket_id: "socket-collider".to_owned(),
    };
    let _live = [named.clone(), sibling.clone(), collider.clone()]
        .iter()
        .map(|target| {
            fixture
                .runtime
                .layout_applies()
                .register_target(target.clone())
                .expect("each live generation registers")
        })
        .collect::<Vec<_>>();

    let seen: Arc<Mutex<Vec<UiBusMsg>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let acknowledging = named.clone();
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
            runtime
                .layout_applies()
                .accept_result(&acknowledging, &applied(correlation_id));
        });

    let response = handle_ui_apply_layout(
        &fixture.core,
        &fixture.caller('a'),
        apply_request(&named.fingerprint, &named.tab_id, SESSION_ID),
    )
    .await
    .expect("a fenced apply against a live generation is admitted");
    drop(subscription);

    assert_eq!(response.body.outcome, proto::UiApplyLayoutOutcome::Applied);
    assert!(
        !response.body.correlation_id.is_empty(),
        "the caller is told which correlation its target answered"
    );
    assert_eq!(response.body.reason, None);

    let published = seen.lock().expect("the bus sink lock").clone();
    assert_eq!(published.len(), 1, "one apply is published: {published:?}");
    let message = published.first().expect("exactly one apply is published");
    let UiBusMsg::Apply {
        target_tab_id,
        target_socket_id,
        correlation_id,
        ..
    } = message
    else {
        panic!("an apply publishes an apply message");
    };
    assert_eq!(target_tab_id, "tab-1");
    assert_eq!(
        target_socket_id, "socket-named",
        "the apply is pinned to the exact socket generation"
    );
    assert_eq!(*correlation_id, response.body.correlation_id);

    assert!(
        ui_bus_frame(message, &UiViewer::browser("socket-named")).is_some(),
        "the named generation receives the apply"
    );
    for (label, viewer) in [
        (
            "the sibling tab's generation",
            UiViewer::browser("socket-sibling"),
        ),
        (
            "a colliding tab id on another device",
            UiViewer::browser("socket-collider"),
        ),
        (
            "a browser on no generation",
            UiViewer {
                browser_ui: true,
                socket_id: None,
            },
        ),
        ("a feed with no UI capability", UiViewer::suppressed()),
    ] {
        assert!(
            ui_bus_frame(message, &viewer).is_none(),
            "{label} must not receive an apply aimed elsewhere"
        );
    }
    assert_eq!(
        fixture.runtime.layout_applies().stats(),
        LayoutApplyOwnerStats {
            targets: 3,
            pending: 0
        },
        "the apply named one live generation and settled on it"
    );
}

#[tokio::test]
async fn an_unfenced_ui_list_states_is_refused_rather_than_answered() {
    let fixture = UiStateFixture::new("tab-fence-list").await;
    let fenced = fixture.caller('a');
    handle_ui_report_state(
        &fixture.core,
        &fenced,
        report_request("tab-1", "/s/one", Some(layout_document(SESSION_ID))),
    )
    .await
    .expect("a fenced report is retained");

    let refused = handle_ui_list_states(
        &fixture.core,
        &fixture.caller_without_tab('a'),
        proto::UiListStatesRequest::default(),
    )
    .await
    .expect_err("a request that carried no tab id is refused before the table is read");
    assert_eq!(refused.code, ErrorCode::FailedPrecondition);
    let reason = refused.message.as_deref().unwrap_or_default();
    assert!(
        reason.contains("tab fence"),
        "the refusal names the fence it is missing: {reason}"
    );
    assert!(
        reason.contains("x-roost-tab-id"),
        "the refusal names the header that carries it: {reason}"
    );

    // The fence is the only difference between the two calls, so the same device
    // WITH a tab id reads the very row the refusal withheld.
    let listed = handle_ui_list_states(
        &fixture.core,
        &fenced,
        proto::UiListStatesRequest::default(),
    )
    .await
    .expect("a fenced caller is answered");
    let tabs: Vec<&str> = listed
        .body
        .tabs
        .iter()
        .map(|tab| tab.tab_id.as_str())
        .collect();
    assert_eq!(
        tabs,
        vec!["tab-1"],
        "the table is not empty, so the refusal above was about the fence"
    );
    assert!(
        listed.body.tabs[0]
            .state
            .as_option()
            .and_then(|state| state.layout_document.as_option())
            .is_some(),
        "the retained report carries the document a layout apply is aimed at"
    );
}

#[tokio::test]
async fn an_apply_naming_a_dead_generation_is_settled_and_never_broadcast() {
    let fixture = UiStateFixture::new("tab-fence-dead-generation").await;
    let first = UiLayoutApplyTarget {
        fingerprint: browser_fingerprint('a'),
        tab_id: "tab-1".to_owned(),
        socket_id: "socket-1".to_owned(),
    };
    let first_fingerprint = first.fingerprint.clone();
    let first_tab_id = first.tab_id.clone();
    let first_guard = fixture
        .runtime
        .layout_applies()
        .register_target(first.clone())
        .expect("a live generation registers");

    let (resolution, published_while_live) = collect_ui_bus(&fixture, async {
        let requested = fixture
            .runtime
            .layout_applies()
            .request_apply(&first_fingerprint, &first_tab_id, |publication| {
                publish_reserved_apply(&fixture.core, publication);
            })
            .expect("a live generation admits the apply");
        let LayoutApplyRequest::Pending(pending) = requested else {
            panic!("a registered generation is reservable");
        };
        // The socket closes with the apply still in flight.
        drop(first_guard);
        pending.await_resolution().await
    })
    .await;
    assert_eq!(resolution.outcome, proto::UiApplyLayoutOutcome::TargetGone);
    assert_eq!(
        resolution.reason.as_deref(),
        Some(UI_LAYOUT_TARGET_GONE_REASON)
    );
    assert_eq!(
        published_while_live.len(),
        1,
        "the apply went out while the generation was live"
    );
    assert!(
        ui_bus_frame(
            published_while_live.first().expect("the published apply"),
            &UiViewer::browser("socket-1")
        )
        .is_some(),
        "a dead generation's own apply reached it, and only it"
    );

    // With the socket gone the tab is not reservable, and nothing is published.
    let (closed_request, after_close) = collect_ui_bus(&fixture, async {
        fixture
            .runtime
            .layout_applies()
            .request_apply(&first_fingerprint, &first_tab_id, |_| {})
    })
    .await;
    let closed_request = closed_request.expect("a closed generation is not a capacity refusal");
    let LayoutApplyRequest::TargetGone(closed) = closed_request else {
        panic!("a generation that closed admits nothing");
    };
    assert_eq!(closed.outcome, proto::UiApplyLayoutOutcome::TargetGone);
    assert_eq!(closed.reason.as_deref(), Some(UI_LAYOUT_TARGET_GONE_REASON));
    assert!(
        after_close.is_empty(),
        "a dead generation is not broadcast to: {after_close:?}"
    );

    // The tab redials on a new socket, and the closed one can neither receive
    // nor acknowledge its successor's apply.
    let second = UiLayoutApplyTarget {
        fingerprint: first_fingerprint.clone(),
        tab_id: first_tab_id.clone(),
        socket_id: "socket-2".to_owned(),
    };
    let _second_guard = fixture
        .runtime
        .layout_applies()
        .register_target(second.clone())
        .expect("the redialled generation registers");
    let (successor_request, successor_publications) = collect_ui_bus(&fixture, async {
        fixture.runtime.layout_applies().request_apply(
            &second.fingerprint,
            &second.tab_id,
            |publication| {
                publish_reserved_apply(&fixture.core, publication);
            },
        )
    })
    .await;
    let successor_request = successor_request.expect("the redialled generation is reservable");
    let LayoutApplyRequest::Pending(pending) = successor_request else {
        panic!("the registered redialled generation is reservable");
    };
    assert_eq!(successor_publications.len(), 1);
    let successor_apply = successor_publications
        .first()
        .expect("exactly one apply is published");
    assert!(
        ui_bus_frame(successor_apply, &UiViewer::browser("socket-2")).is_some(),
        "the live generation receives its own apply"
    );
    assert!(
        ui_bus_frame(successor_apply, &UiViewer::browser("socket-1")).is_none(),
        "the generation that closed receives nothing"
    );

    let correlation_id = pending.correlation_id().to_owned();
    assert!(
        !fixture
            .runtime
            .layout_applies()
            .accept_result(&first, &applied(&correlation_id)),
        "a closed generation cannot acknowledge the apply reserved after it"
    );
    assert_eq!(
        fixture.runtime.layout_applies().stats().pending,
        1,
        "the reservation is still the live generation's to answer"
    );
    assert!(
        fixture
            .runtime
            .layout_applies()
            .accept_result(&second, &applied(&correlation_id))
    );
    assert_eq!(
        pending.await_resolution().await.outcome,
        proto::UiApplyLayoutOutcome::Applied
    );
    assert_eq!(
        fixture.runtime.layout_applies().stats().pending,
        0,
        "one reservation, settled once"
    );
}
