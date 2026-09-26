//! A layout acknowledgement travelling the Sync path: a tab socket registers as
//! the live apply target, a reserved apply is answered by a real client frame,
//! and the frame settles the reservation the RPC is holding open.
//!
//! The fence is the point. `UiApplyLayout` blocks on a target that the browser
//! may never answer, so the acknowledgement is accepted only from the exact
//! `(fingerprint, tab, socket)` the apply was reserved against. A socket that
//! redialled is a DIFFERENT socket id, and settling from it would hand a
//! caller a success for an apply its predecessor was asked to perform.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use roost_coord::sync_ws::commands::{ClientContext, CommandOutcome, handle_client_frame};
use roost_coord::sync_ws::commands_layout::{register_layout_target, settle_layout_result};
use roost_coord::sync_ws::domain_table::DomainGenerations;
use roost_coord::sync_ws::session::SyncV2Session;
use roost_coord::sync_ws::snapshot_registry::SnapshotTokenRegistry;
use roost_coord::ui_state::UiStateRuntime;
use roost_coord::ui_state::layout_apply::LayoutApplyRequest;
use roost_proto::__buffa::oneof::sync_client_frame::Command as ClientCommand;
use roost_proto::{SyncClientFrame, UiApplyLayoutOutcome, UiApplyLayoutResult};

const FINGERPRINT: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const TAB_ID: &str = "tab-1";

/// A runtime whose clock never moves, so an unsettled apply is distinguishable
/// from one that merely has not been awaited yet.
fn runtime() -> UiStateRuntime {
    UiStateRuntime::with_clock(Arc::new(|| 0_i64))
}

fn context() -> ClientContext {
    ClientContext {
        read_only: false,
        tab_id: Some(TAB_ID.to_owned()),
        viewer_key: Some(format!("{FINGERPRINT}:{TAB_ID}")),
        fingerprint: FINGERPRINT.to_owned(),
        session_ids: std::collections::BTreeSet::new(),
    }
}

fn session(socket_id: &str) -> SyncV2Session {
    SyncV2Session::new(
        socket_id.to_owned(),
        Arc::new(DomainGenerations::new(1_000)),
        true,
    )
}

/// The frame a browser sends when it has performed the apply it was handed.
fn result_frame(socket_id: &str, correlation_id: &str) -> SyncClientFrame {
    SyncClientFrame {
        ack_delivery_seq: None,
        socket_id: socket_id.to_owned(),
        command: Some(ClientCommand::UiApplyLayoutResult(Box::new(
            UiApplyLayoutResult {
                correlation_id: correlation_id.to_owned(),
                outcome: UiApplyLayoutOutcome::Applied.into(),
                ..Default::default()
            },
        ))),
        __buffa_unknown_fields: Default::default(),
    }
}

fn applied_frame(correlation_id: &str) -> SyncClientFrame {
    result_frame("socket-1", correlation_id)
}

/// A fenced device whose tab socket is the registered target, an apply reserved
/// on it, and the browser's own frame settling it.
#[tokio::test]
async fn a_registered_tabs_socket_settles_the_apply_reserved_on_it() {
    let runtime = runtime();
    let context = context();
    let guard = register_layout_target(&runtime, &context, "socket-1")
        .expect("the tab socket registers")
        .expect("a writable tab socket has a target");
    assert_eq!(runtime.layout_applies().stats().targets, 1);

    let reserved = runtime
        .layout_applies()
        .request_apply(FINGERPRINT, TAB_ID, |_| {})
        .expect("a live target admits the apply");
    let LayoutApplyRequest::Pending(pending) = reserved else {
        panic!("a registered target is live, so the apply is published, not target-gone");
    };
    let correlation_id = pending.correlation_id().to_owned();

    let mut session = session("socket-1");
    let mut tokens = SnapshotTokenRegistry::new();
    let frame = applied_frame(&correlation_id);
    let outcome = handle_client_frame(&mut session, &context, &frame, &mut tokens, 1_000);
    assert!(
        matches!(&outcome, CommandOutcome::LayoutResult { tab_id, .. } if tab_id == TAB_ID),
        "a layout result goes back to the layout-apply owner, not the terminal lane"
    );

    assert!(
        settle_layout_result(&runtime, &context, "socket-1", &outcome),
        "the frame came from the registered target, so it settles"
    );
    let resolution = pending.await_resolution().await;
    assert_eq!(resolution.outcome, UiApplyLayoutOutcome::Applied);
    assert_eq!(resolution.correlation_id, correlation_id);
    assert_eq!(resolution.reason, None);
    assert_eq!(runtime.layout_applies().stats().pending, 0);
    drop(guard);
}

/// The redial fence: the same tab, the same browser, a different socket
/// generation. Its frame settles nothing, and the caller keeps waiting until
/// the reservation's own deadline.
#[tokio::test]
async fn a_redialled_socket_settles_nothing_the_predecessor_reserved() {
    let runtime = runtime();
    let context = context();
    let _guard = register_layout_target(&runtime, &context, "socket-1")
        .expect("the first socket registers")
        .expect("a writable tab socket has a target");

    let reserved = runtime
        .layout_applies()
        .request_apply(FINGERPRINT, TAB_ID, |_| {})
        .expect("a live target admits the apply");
    let LayoutApplyRequest::Pending(pending) = reserved else {
        panic!("a registered target is live");
    };
    let correlation_id = pending.correlation_id().to_owned();

    let mut redialled = session("socket-2");
    let mut tokens = SnapshotTokenRegistry::new();
    let frame = result_frame("socket-2", &correlation_id);
    let outcome = handle_client_frame(&mut redialled, &context, &frame, &mut tokens, 1_000);
    assert!(
        matches!(&outcome, CommandOutcome::LayoutResult { .. }),
        "the frame itself is legal for the socket that sent it"
    );

    assert!(
        !settle_layout_result(&runtime, &context, "socket-2", &outcome),
        "the apply was reserved on socket-1, and socket-2 is not it"
    );
    assert_eq!(
        runtime.layout_applies().stats().pending,
        1,
        "the caller is still waiting, which is the only honest answer"
    );
    // The reservation is dropped, not awaited: its own deadline is the only
    // thing that may retire it now, which is what a caller blocked on an
    // unanswered apply is entitled to.
    drop(pending);
}

/// A result whose correlation this owner never issued settles nothing: the
/// ledger has no reservation to hand back, and inventing one would resolve a
/// caller's apply with a stranger's outcome.
#[tokio::test]
async fn a_result_for_a_correlation_nobody_issued_settles_nothing() {
    let runtime = runtime();
    let context = context();
    let _guard = register_layout_target(&runtime, &context, "socket-1")
        .expect("the tab socket registers")
        .expect("a writable tab socket has a target");

    let mut live = session("socket-1");
    let mut tokens = SnapshotTokenRegistry::new();
    let frame = applied_frame("correlation-nobody-issued");
    let outcome = handle_client_frame(&mut live, &context, &frame, &mut tokens, 1_000);

    assert!(!settle_layout_result(
        &runtime, &context, "socket-1", &outcome
    ));
    assert_eq!(runtime.layout_applies().stats().pending, 0);
}

/// A socket with no tab has no layout to be sent, so it registers no target
/// rather than one nothing can ever reserve against.
#[test]
fn a_socket_without_a_tab_registers_no_target() {
    let runtime = runtime();
    let mut tabless = context();
    tabless.tab_id = None;
    assert!(
        register_layout_target(&runtime, &tabless, "socket-1")
            .expect("a tab-less socket is not a capacity failure")
            .is_none()
    );
    assert_eq!(runtime.layout_applies().stats().targets, 0);

    let mut read_only = context();
    read_only.read_only = true;
    assert!(
        register_layout_target(&runtime, &read_only, "socket-1")
            .expect("a read-only socket is not a capacity failure")
            .is_none(),
        "a worker may acknowledge and subscribe but never write, and an apply writes"
    );
}

/// The settlement call is safe to make for every frame, which is what lets the
/// socket driver call it once per outcome instead of matching on the variant.
#[test]
fn an_outcome_that_is_not_a_layout_result_settles_nothing() {
    let runtime = runtime();
    let context = context();
    let _guard = register_layout_target(&runtime, &context, "socket-1")
        .expect("the tab socket registers")
        .expect("a writable tab socket has a target");

    assert!(!settle_layout_result(
        &runtime,
        &context,
        "socket-1",
        &CommandOutcome::Acknowledged { released: 3 },
    ));
    assert!(!settle_layout_result(
        &runtime,
        &context,
        "socket-1",
        &CommandOutcome::Nothing,
    ));
}
