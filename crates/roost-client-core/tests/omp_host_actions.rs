//! The browser's receipt-tracked action queue: serialisation, the transcript
//! fence, stale-view refusal, cancellation, and native failures releasing the
//! next action without deadlocking modal commands.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::client::agent_chat::{
    MAX_PENDING_OMP_ACTIONS, OmpActionClass, OmpActionIdentity, OmpActionQueue, OmpReceiptOutcome,
    OmpSubmit, OmpUiActionReceipt,
};

const CONVERSATION: &str = "conversation-a";
const VIEW: &str = "view-a";
const TRANSPORT: &str = "transport-a";

fn identity(transport: &str) -> OmpActionIdentity {
    OmpActionIdentity {
        conversation_id: CONVERSATION.to_owned(),
        view_id: VIEW.to_owned(),
        transport_id: transport.to_owned(),
    }
}

fn action(ev: &str) -> serde_json::Value {
    serde_json::json!({"method":"event","params":{"event":{"ev":ev}}})
}

#[test]
fn receipt_tracked_actions_serialise_while_edits_and_controls_bypass() {
    assert_eq!(
        OmpActionClass::Bypass,
        roost_client_core::client::agent_chat::classify_action(&action("edit"))
    );
    assert_eq!(
        OmpActionClass::Bypass,
        roost_client_core::client::agent_chat::classify_action(&action("focus"))
    );
    assert_eq!(
        OmpActionClass::ReceiptTracked,
        roost_client_core::client::agent_chat::classify_action(
            &serde_json::json!({"method":"input"})
        )
    );
    let mut queue = OmpActionQueue::new();
    assert!(matches!(
        queue.submit("edit-1".into(), identity(TRANSPORT), action("edit")),
        OmpSubmit::Bypass(_)
    ));
    assert!(matches!(
        queue.submit("request-1".into(), identity(TRANSPORT), action("click")),
        OmpSubmit::Send(_)
    ));
    assert_eq!(
        queue.submit("request-2".into(), identity(TRANSPORT), action("click")),
        OmpSubmit::Queued
    );
    assert_eq!(queue.pending_count(), 2);
}

#[test]
fn a_receipt_below_the_transcript_fence_is_deferred_and_released_later() {
    let mut queue = OmpActionQueue::new();
    let OmpSubmit::Send(_) = queue.submit("request-1".into(), identity(TRANSPORT), action("click"))
    else {
        panic!("the first action is in flight");
    };
    let receipt = OmpUiActionReceipt {
        origin_request_id: "request-1".to_owned(),
        conversation_id: CONVERSATION.to_owned(),
        view_id: VIEW.to_owned(),
        transport_id: TRANSPORT.to_owned(),
        after_seq: 30,
        error: String::new(),
    };
    assert_eq!(queue.receive(&receipt, 29), OmpReceiptOutcome::Deferred);
    assert_eq!(queue.pending_count(), 1);
    assert!(queue.settle(29).is_none());
    assert!(matches!(
        queue.settle(30),
        Some(OmpReceiptOutcome::Completed { error, .. }) if error.is_empty()
    ));
    assert_eq!(queue.pending_count(), 0);
}

#[test]
fn a_stale_or_wrong_view_receipt_cannot_release_the_in_flight_action() {
    let mut queue = OmpActionQueue::new();
    queue.submit("request-1".into(), identity(TRANSPORT), action("click"));
    let mut receipt = OmpUiActionReceipt {
        origin_request_id: "request-1".to_owned(),
        conversation_id: CONVERSATION.to_owned(),
        view_id: "other-view".to_owned(),
        transport_id: TRANSPORT.to_owned(),
        after_seq: 1,
        error: String::new(),
    };
    assert_eq!(queue.receive(&receipt, 100), OmpReceiptOutcome::Ignored);
    receipt.view_id = VIEW.to_owned();
    receipt.transport_id = "stale-transport".to_owned();
    assert_eq!(queue.receive(&receipt, 100), OmpReceiptOutcome::Ignored);
    receipt.transport_id = TRANSPORT.to_owned();
    receipt.origin_request_id = "another-request".to_owned();
    assert_eq!(queue.receive(&receipt, 100), OmpReceiptOutcome::Ignored);
    assert_eq!(queue.pending_count(), 1);
}

#[test]
fn a_failed_native_action_releases_the_queue_with_its_error() {
    let mut queue = OmpActionQueue::new();
    queue.submit("request-1".into(), identity(TRANSPORT), action("click"));
    queue.submit("request-2".into(), identity(TRANSPORT), action("click"));
    let receipt = OmpUiActionReceipt {
        origin_request_id: "request-1".to_owned(),
        conversation_id: CONVERSATION.to_owned(),
        view_id: VIEW.to_owned(),
        transport_id: TRANSPORT.to_owned(),
        after_seq: 3,
        error: "controller refused the event".to_owned(),
    };
    assert!(matches!(
        queue.receive(&receipt, 3),
        OmpReceiptOutcome::Completed {
            request_id,
            error,
            next,
        } if request_id == "request-1"
            && error == "controller refused the event"
            && (*next).as_ref().is_some_and(|next| next.request_id.as_str() == "request-2")
    ));
}

#[test]
fn cancelling_a_view_drops_every_queued_action_and_its_fence() {
    let mut queue = OmpActionQueue::new();
    queue.submit("request-1".into(), identity(TRANSPORT), action("click"));
    queue.submit("request-2".into(), identity(TRANSPORT), action("click"));
    queue.cancel_all();
    assert_eq!(queue.pending_count(), 0);
    assert!(queue.settle(100).is_none());
    assert_eq!(queue.pending_count(), 0);
}

#[test]
fn the_action_queue_is_bounded() {
    let mut queue = OmpActionQueue::new();
    for index in 0..=MAX_PENDING_OMP_ACTIONS {
        let submit = queue.submit(
            format!("request-{index}"),
            identity(TRANSPORT),
            action("click"),
        );
        if index < MAX_PENDING_OMP_ACTIONS {
            assert!(matches!(submit, OmpSubmit::Send(_) | OmpSubmit::Queued));
        } else {
            assert_eq!(submit, OmpSubmit::Full);
        }
    }
}

#[test]
fn plain_typeahead_coalesces_without_crossing_native_key_or_paste_boundaries() {
    let input = |data: &str| serde_json::json!({"method":"input","params":{"data":data}});
    let mut queue = OmpActionQueue::new();
    queue.submit("first".into(), identity(TRANSPORT), action("click"));
    queue.submit("text-a".into(), identity(TRANSPORT), input("hello "));
    queue.submit("text-b".into(), identity(TRANSPORT), input("世界🔑"));
    queue.submit("left".into(), identity(TRANSPORT), input("\x1b[D"));
    queue.submit(
        "paste".into(),
        identity(TRANSPORT),
        input("\x1b[200~pasted\x1b[201~"),
    );
    let receipt = |id: &str| OmpUiActionReceipt {
        origin_request_id: id.into(),
        conversation_id: CONVERSATION.into(),
        view_id: VIEW.into(),
        transport_id: TRANSPORT.into(),
        after_seq: 0,
        error: String::new(),
    };
    let OmpReceiptOutcome::Completed { next, .. } = queue.receive(&receipt("first"), 0) else {
        panic!("release text");
    };
    let Some(text) = *next else {
        panic!("release text");
    };
    assert_eq!(text.request_id, "text-a");
    assert_eq!(text.action, input("hello 世界🔑"));
    let OmpReceiptOutcome::Completed { next, .. } = queue.receive(&receipt("text-a"), 0) else {
        panic!("release key");
    };
    let Some(left) = *next else {
        panic!("release key");
    };
    assert_eq!(left.action, input("\x1b[D"));
    let OmpReceiptOutcome::Completed { next, .. } = queue.receive(&receipt("left"), 0) else {
        panic!("release paste");
    };
    let Some(paste) = *next else {
        panic!("release paste");
    };
    assert_eq!(paste.action, input("\x1b[200~pasted\x1b[201~"));
}

#[test]
fn ambiguous_admission_failure_cancels_later_actions_without_replaying_them() {
    let mut queue = OmpActionQueue::new();
    queue.submit("first".into(), identity(TRANSPORT), action("send"));
    queue.submit("later".into(), identity(TRANSPORT), action("send"));
    assert!(!queue.reject_admission("first", &identity("old-transport")));
    assert_eq!(queue.pending_count(), 2);
    assert!(queue.reject_admission("first", &identity(TRANSPORT)));
    assert!(!queue.has_pending_input());
    assert_eq!(queue.completed_count(), 0);
}

#[test]
fn no_op_receipt_completes_only_after_its_transcript_fence_and_only_once() {
    let mut queue = OmpActionQueue::new();
    queue.submit("no-op".into(), identity(TRANSPORT), action("send"));
    let receipt = OmpUiActionReceipt {
        origin_request_id: "no-op".into(),
        conversation_id: CONVERSATION.into(),
        view_id: VIEW.into(),
        transport_id: TRANSPORT.into(),
        after_seq: 5,
        error: String::new(),
    };
    assert_eq!(queue.receive(&receipt, 4), OmpReceiptOutcome::Deferred);
    assert_eq!(queue.completed_count(), 0);
    assert!(matches!(
        queue.settle(5),
        Some(OmpReceiptOutcome::Completed { .. })
    ));
    assert_eq!(queue.completed_count(), 1);
    assert_eq!(queue.receive(&receipt, 5), OmpReceiptOutcome::Ignored);
    assert_eq!(queue.completed_count(), 1);
}

#[test]
fn a_restarted_native_transport_drops_old_typeahead_before_new_input() {
    let mut queue = OmpActionQueue::new();
    queue.submit("old-send".into(), identity(TRANSPORT), action("send"));
    queue.submit(
        "old-typeahead".into(),
        identity(TRANSPORT),
        serde_json::json!({"method":"input","params":{"data":"old message"}}),
    );
    queue.retain_identity(&identity(TRANSPORT));
    assert_eq!(queue.pending_count(), 2);
    queue.retain_identity(&identity("new-native-transport"));
    assert!(!queue.has_pending_input());
    assert_eq!(queue.completed_count(), 0);
    let OmpSubmit::Send(next) = queue.submit(
        "new-input".into(),
        identity("new-native-transport"),
        action("send"),
    ) else {
        panic!("new owner must not await the old receipt");
    };
    assert_eq!(next.request_id, "new-input");
}
