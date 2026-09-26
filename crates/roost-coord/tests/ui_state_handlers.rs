//! The report, list and dispatch methods over a real migrated database: the
//! tab fence, the persisted-session fence, and the bus messages each publishes.
//!
//! `DevicePlusFence` has two halves and each is load-bearing on its own. A
//! request with no tab id cannot be attributed to one live socket, and a request
//! naming a session with no `sessions` row would put a command on the bus for a
//! pane the fleet has never heard of -- so both are asserted here per method,
//! because "all four" is exactly the property a per-method port drops.

// A test that cannot say what it expected is not a test. `expect` is denied
// outside `#[cfg(test)]`, and an integration test is its own crate, so the
// exemption has to be stated here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod ui_state_fixture;

use connectrpc::ErrorCode;
use roost_coord::events::bus_messages::UiBusMsg;
use roost_coord::ui_state::rpc::{
    handle_ui_dispatch, handle_ui_list_states, handle_ui_report_state,
};
use roost_proto as proto;
use roost_proto::buffa::MessageField;
use roost_proto::__buffa::oneof::ui_command::Command;
use ui_state_fixture::{
    FOREIGN_SESSION_ID, SESSION_ID, UiStateFixture, browser_fingerprint, collect_ui_bus,
    layout_document, report_request, select_tab_command,
};

#[tokio::test]
async fn report_list_and_dispatch_refuse_a_request_that_carried_no_tab_id() {
    let fixture = UiStateFixture::new("no-tab").await;
    let caller = fixture.caller_without_tab('a');

    let report = handle_ui_report_state(
        &fixture.core,
        &fixture.runtime,
        &caller,
        report_request("tab-1", "/s/one", None),
    )
    .await
    .expect_err("a report with no tab id is refused");
    assert_eq!(report.code, ErrorCode::FailedPrecondition);
    assert!(
        report.message.as_deref().unwrap_or_default().contains("tab fence"),
        "the refusal must name the fence it is missing: {}",
        report.message.as_deref().unwrap_or_default()
    );

    let list = handle_ui_list_states(
        &fixture.core,
        &fixture.runtime,
        &caller,
        proto::UiListStatesRequest::default(),
    )
    .await
    .expect_err("a list with no tab id is refused");
    assert!(
        list.message.as_deref().unwrap_or_default().contains("tab fence"),
        "UiListStates must refuse the same way: {}",
        list.message.as_deref().unwrap_or_default()
    );

    let dispatch = handle_ui_dispatch(
        &fixture.core,
        &fixture.runtime,
        &caller,
        proto::UiDispatchRequest {
            target_tab_id: "tab-1".to_owned(),
            command: MessageField::some(select_tab_command(SESSION_ID)),
            ..Default::default()
        },
    )
    .await
    .expect_err("a dispatch with no tab id is refused");
    assert!(
        dispatch.message.as_deref().unwrap_or_default().contains("tab fence"),
        "UiDispatch must refuse the same way: {}",
        dispatch.message.as_deref().unwrap_or_default()
    );

    assert_eq!(
        fixture.runtime.states().retained_count(),
        0,
        "a refused request retained nothing"
    );
}

#[tokio::test]
async fn a_report_is_retained_listed_and_published_under_the_callers_own_fingerprint() {
    let fixture = UiStateFixture::new("report").await;
    let caller = fixture.caller('a');

    let (response, messages) = collect_ui_bus(&fixture, async {
        handle_ui_report_state(
            &fixture.core,
            &fixture.runtime,
            &caller,
            report_request("tab-1", "/s/one", Some(layout_document(SESSION_ID))),
        )
        .await
    })
    .await;
    response.expect("a well-formed report is admitted");

    let published = messages
        .first()
        .expect("the report publishes exactly one message");
    let UiBusMsg::State { fp, tab_id, state } = published else {
        panic!("a report publishes a state message, got {published:?}");
    };
    assert_eq!(fp, &browser_fingerprint('a'));
    assert_eq!(tab_id, "tab-1");
    assert_eq!(state.active_path, "/s/one");

    let listed = handle_ui_list_states(
        &fixture.core,
        &fixture.runtime,
        &caller,
        proto::UiListStatesRequest::default(),
    )
    .await
    .expect("a list is admitted")
    .body;
    assert_eq!(listed.tabs.len(), 1);
    assert_eq!(listed.tabs[0].fp, browser_fingerprint('a'));
    assert_eq!(listed.tabs[0].tab_id, "tab-1");
    assert_eq!(
        listed.tabs[0].label, "Chrome - test",
        "the label comes from the key row, not from the report"
    );
    let document = listed.tabs[0]
        .state
        .as_option()
        .and_then(|state| state.layout_document.as_option())
        .expect("the reported document is retained");
    assert_eq!(document.bindings[0].session_id, SESSION_ID);
}

#[tokio::test]
async fn a_reported_tab_id_never_borrows_another_devices_fingerprint() {
    let fixture = UiStateFixture::new("identity").await;
    handle_ui_report_state(
        &fixture.core,
        &fixture.runtime,
        &fixture.caller('a'),
        report_request("tab-1", "/s/one", None),
    )
    .await
    .expect("the first report is admitted");
    handle_ui_report_state(
        &fixture.core,
        &fixture.runtime,
        &fixture.caller('b'),
        report_request("tab-1", "/s/two", None),
    )
    .await
    .expect("the second report is admitted");

    let entries = fixture.runtime.states().list();
    assert_eq!(entries.len(), 2, "the same tab id on two devices is two reports");
    assert_eq!(entries[0].fingerprint, browser_fingerprint('a'));
    assert_eq!(entries[1].fingerprint, browser_fingerprint('b'));
}

#[tokio::test]
async fn a_report_naming_a_session_with_no_row_is_refused_and_retains_nothing() {
    let fixture = UiStateFixture::new("report-foreign").await;
    let (result, messages) = collect_ui_bus(&fixture, async {
        handle_ui_report_state(
            &fixture.core,
            &fixture.runtime,
            &fixture.caller('a'),
            report_request("tab-1", "/s/one", Some(layout_document(FOREIGN_SESSION_ID))),
        )
        .await
    })
    .await;

    let refused = result.expect_err("a report naming an unpersisted session is refused");
    assert_eq!(refused.code, ErrorCode::NotFound);
    assert_eq!(
        fixture.runtime.states().retained_count(),
        0,
        "the refused report retained nothing"
    );
    assert!(
        messages.is_empty(),
        "the refused report published nothing, saw {messages:?}"
    );
}

#[tokio::test]
async fn a_report_cannot_impersonate_another_browser_by_naming_its_tab() {
    let fixture = UiStateFixture::new("impersonate").await;
    handle_ui_report_state(
        &fixture.core,
        &fixture.runtime,
        &fixture.caller('a'),
        report_request("tab-1", "/s/one", None),
    )
    .await
    .expect("the first report is admitted");

    // The attacker's request is otherwise well formed; only the fence is wrong.
    let attacker = ui_state_fixture::browser_caller(
        &browser_fingerprint('b'),
        &fixture.account_id,
        None,
    );
    let refused = handle_ui_report_state(
        &fixture.core,
        &fixture.runtime,
        &attacker,
        report_request("tab-1", "/s/hijacked", None),
    )
    .await
    .expect_err("a report with no tab fence is refused whatever it names");
    assert_eq!(refused.code, ErrorCode::FailedPrecondition);
    assert_eq!(fixture.runtime.states().list()[0].state.active_path, "/s/one");
}

#[tokio::test]
async fn an_oversized_report_field_is_refused_before_anything_is_retained() {
    let fixture = UiStateFixture::new("report-bounds").await;
    let oversized = "x".repeat(roost_coord::ui_state::limits::UI_TAB_ID_MAX_UTF8_BYTES + 1);
    let refused = handle_ui_report_state(
        &fixture.core,
        &fixture.runtime,
        &fixture.caller('a'),
        report_request(&oversized, "/", None),
    )
    .await
    .expect_err("an over-long tab id is refused");
    assert_eq!(refused.code, ErrorCode::InvalidArgument);
    assert_eq!(fixture.runtime.states().retained_count(), 0);
}

#[tokio::test]
async fn a_dispatch_publishes_the_canonical_command_and_counts_listeners() {
    let fixture = UiStateFixture::new("dispatch").await;
    let (response, messages) = collect_ui_bus(&fixture, async {
        handle_ui_dispatch(
            &fixture.core,
            &fixture.runtime,
            &fixture.caller('a'),
            proto::UiDispatchRequest {
                target_tab_id: "tab-1".to_owned(),
                command: MessageField::some(select_tab_command(SESSION_ID)),
                ..Default::default()
            },
        )
        .await
    })
    .await;
    let response = response.expect("a well-formed dispatch is admitted");
    assert_eq!(
        response.body.delivered, 1,
        "the collector itself is the one live subscriber"
    );
    let published = messages.first().expect("one command is published");
    let UiBusMsg::Command {
        target_tab_id,
        command,
    } = published
    else {
        panic!("a dispatch publishes a command message, got {published:?}");
    };
    assert_eq!(target_tab_id, "tab-1");
    let Some(Command::SelectTab(select)) = command.command.as_ref() else {
        panic!("the canonical command is still a selectTab");
    };
    assert_eq!(select.session_id, SESSION_ID);
}

#[tokio::test]
async fn a_dispatch_naming_an_unpersisted_session_publishes_nothing() {
    let fixture = UiStateFixture::new("dispatch-foreign").await;
    let (result, messages) = collect_ui_bus(&fixture, async {
        handle_ui_dispatch(
            &fixture.core,
            &fixture.runtime,
            &fixture.caller('a'),
            proto::UiDispatchRequest {
                target_tab_id: "tab-1".to_owned(),
                command: MessageField::some(select_tab_command(FOREIGN_SESSION_ID)),
                ..Default::default()
            },
        )
        .await
    })
    .await;
    let refused = result.expect_err("a dispatch naming an unpersisted session is refused");
    assert_eq!(refused.code, ErrorCode::NotFound);
    assert!(messages.is_empty(), "nothing reached the bus: {messages:?}");
}

#[tokio::test]
async fn a_dispatch_refuses_apply_layout_and_an_empty_command() {
    let fixture = UiStateFixture::new("dispatch-apply").await;
    let apply_command = proto::UiCommand {
        command: Some(Command::ApplyLayout(Box::new(proto::UiApplyLayout {
            document: MessageField::some(layout_document(SESSION_ID)),
            ..Default::default()
        }))),
        ..Default::default()
    };
    let (refused, messages) = collect_ui_bus(&fixture, async {
        handle_ui_dispatch(
            &fixture.core,
            &fixture.runtime,
            &fixture.caller('a'),
            proto::UiDispatchRequest {
                target_tab_id: "tab-1".to_owned(),
                command: MessageField::some(apply_command),
                ..Default::default()
            },
        )
        .await
    })
    .await;
    assert_eq!(
        refused
            .expect_err("applyLayout is not a dispatchable command")
            .code,
        ErrorCode::InvalidArgument
    );
    assert!(messages.is_empty());

    let empty = handle_ui_dispatch(
        &fixture.core,
        &fixture.runtime,
        &fixture.caller('a'),
        proto::UiDispatchRequest::default(),
    )
    .await
    .expect_err("a dispatch with no command is refused");
    assert_eq!(empty.code, ErrorCode::InvalidArgument);
}
