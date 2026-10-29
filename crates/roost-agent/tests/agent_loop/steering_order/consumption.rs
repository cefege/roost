//! A steer may enter the journal after channel draining but before request assembly.
//! Its later notification must not create an extra request or disappear from the UI.
//! Gates hold real runtime boundaries rather than injecting fake consumption events.

use roost_agent::AgentStore;
use std::time::Duration;

use roost_protocol::wire::agent_chat::{TranscriptItem, fold_chat_event};

use super::super::support::{Harness, text_reply, tool_reply, user_texts};

#[tokio::test]
async fn a_steer_consumed_before_its_notification_is_not_requested_twice() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness
        .tools
        .hold_next
        .store(true, std::sync::atomic::Ordering::SeqCst);
    harness.llm.script(
        &id,
        tool_reply(&[("read-call", "read", r#"{"path":"README.md"}"#)]),
    );
    harness
        .llm
        .script(&id, text_reply("The new instruction is included."));
    harness.submit(&id, "Inspect this directory").await;
    tokio::time::timeout(
        Duration::from_secs(5),
        harness.tools.tool_entered.notified(),
    )
    .await
    .expect("the first tool round starts");
    let mut live = harness.runtime.transcript(&id).await.unwrap();
    let event_count = harness.sink.events_for(&id).len();
    let gate = harness.llm.hold_credential_check();
    harness.tools.tool_released.notify_one();
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .expect("the next request is held before input consumption");
    harness
        .runtime
        .submit_request(
            &id,
            "late-before-dispatch",
            "Also inspect the tests".to_owned(),
        )
        .await
        .unwrap();
    gate.released.notify_one();
    let settled = harness.settle(&id).await;
    let requests = harness.llm.requests_for(&id);
    assert_eq!(
        requests.len(),
        2,
        "an already-consumed notification must not continue the run"
    );
    assert_eq!(
        user_texts(&requests[1]),
        vec!["Inspect this directory", "Also inspect the tests"]
    );
    for event in harness.sink.events_for(&id).iter().skip(event_count) {
        fold_chat_event(&mut live, event);
    }
    assert!(settled.queued.is_empty());
    assert!(live.queued.is_empty());
    assert_eq!(
        live.items
            .iter()
            .filter(|item| matches!(item,
        TranscriptItem::User { text, .. } if text == "Also inspect the tests"))
            .count(),
        1,
        "the streamed client must receive the user message without a reload"
    );
}
#[tokio::test]
async fn withdrawing_the_first_input_before_dispatch_makes_no_model_request() {
    let (harness, gate) = Harness::gated();
    let id = harness.conversation().await;
    gate.hold_committed_user_append(1);
    let runtime = harness.runtime.clone();
    let submit_id = id.clone();
    let submit = tokio::spawn(async move {
        runtime
            .submit_request(
                &submit_id,
                "first-input",
                "withdraw before dispatch".to_owned(),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), gate.user_append_held())
        .await
        .expect("first input is durably appended before admission returns");
    // Another replica can withdraw the committed input before admission returns here.
    let withdrawal = harness
        .store
        .admit_command(
            &id,
            roost_agent::CommandAdmission {
                request_id: "withdraw-first".into(),
                kind: "withdraw".into(),
                entries: vec![roost_agent::Entry::QueuedInputWithdrawn {
                    submission_id: "first-input".into(),
                }],
                intent: roost_agent::IntentChange::Withdraw,
                plan_admission: roost_agent::PlanAdmission::AllowPending,
                fence: None,
                expected_intent_seq: None,
                completion_through: None,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        withdrawal,
        roost_agent::AdmitOutcome::Admitted { .. }
    ));
    gate.release_user_append();
    submit.await.unwrap().unwrap();
    let settled = harness.settle(&id).await;
    assert!(settled.items.is_empty());
    assert!(
        harness.llm.requests_for(&id).is_empty(),
        "withdrawn first input must not reach the provider"
    );
}
