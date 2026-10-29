//! A message sent while the agent is still writing a reply waits in the
//! queue and is read after that reply: the live transcript, the journal's
//! transcript and the next model request all put it there.

use super::support::{Harness, text_reply, tool_reply};
use roost_agent::SubmitMode;
use roost_llm::Message;
use roost_protocol::wire::agent_chat::{
    AgentRunState, ChatEvent, Transcript, TranscriptItem, fold_chat_event,
};

#[path = "steering_order/activation.rs"]
mod activation;

#[path = "steering_order/consumption.rs"]
mod consumption;

fn shape(transcript: &Transcript) -> Vec<String> {
    transcript
        .items
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::User { text, .. } => Some(format!("user: {text}")),
            TranscriptItem::Assistant { blocks, .. } => {
                let text = blocks
                    .iter()
                    .filter_map(|block| match block {
                        roost_protocol::wire::agent_chat::TranscriptBlock::Text { text } => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .collect::<String>();
                (!text.is_empty()).then(|| format!("assistant: {text}"))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_message_sent_mid_reply_is_queued_then_answered_after_that_reply() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let gate = harness.llm.hold_stream(&id, 1);
    harness.llm.script(&id, text_reply("first answer"));
    harness.llm.script(&id, text_reply("answer to the steer"));
    harness.submit(&id, "first question").await;
    gate.entered.notified().await;
    harness.submit(&id, "and also this").await;
    assert!(harness.sink.events_for(&id).iter().any(|event| matches!(
        event,
        ChatEvent::Queued { message } if message.text == "and also this"
    )));
    gate.released.notify_one();
    let journal = harness.settle(&id).await;

    let expected = vec![
        "user: first question",
        "assistant: first answer",
        "user: and also this",
        "assistant: answer to the steer",
    ];
    assert_eq!(shape(&journal), expected);

    let mut live = journal.clone();
    live.items.clear();
    for event in harness.sink.events_for(&id) {
        fold_chat_event(&mut live, &event);
    }
    assert_eq!(shape(&live), expected);
    assert!(live.queued.is_empty());

    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 2);
    let tail: Vec<&Message> = requests[1].messages.iter().rev().take(2).collect();
    assert!(
        matches!(tail[0], Message::User { .. }) && matches!(tail[1], Message::Assistant { .. }),
        "the second request must end with the old reply, then the new message"
    );
}

#[tokio::test]
async fn follow_up_waits_for_completion_and_withdrawal_is_idempotent() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness
        .tools
        .hold_next
        .store(true, std::sync::atomic::Ordering::SeqCst);
    harness.llm.script(
        &id,
        tool_reply(&[("call-1", "bash", r#"{"command":"printf current"}"#)]),
    );
    harness.llm.script(&id, text_reply("current answer"));
    harness.llm.script(&id, text_reply("follow-up answer"));
    harness.submit(&id, "current question").await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        harness.tools.tool_entered.notified(),
    )
    .await
    .expect("the current worker tool round starts");
    harness
        .runtime
        .submit_request_with_mode(
            &id,
            "follow-up-1",
            "withdraw this".into(),
            SubmitMode::FollowUp,
        )
        .await
        .unwrap();
    harness
        .runtime
        .submit_request_with_mode(
            &id,
            "follow-up-1",
            "retry text is ignored".into(),
            SubmitMode::Reject,
        )
        .await
        .unwrap();
    harness
        .runtime
        .submit_request_with_mode(
            &id,
            "follow-up-2",
            "answer after completion".into(),
            SubmitMode::FollowUp,
        )
        .await
        .unwrap();
    let queued = harness.runtime.transcript(&id).await.unwrap().queued;
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[0].id, "follow-up-1");
    assert_eq!(queued[0].mode, "follow_up");

    assert!(
        harness
            .runtime
            .withdraw(&id, "withdraw-1", "follow-up-1")
            .await
            .unwrap()
    );
    assert!(
        !harness
            .runtime
            .withdraw(&id, "withdraw-1", "follow-up-1")
            .await
            .unwrap()
    );
    assert_eq!(
        harness.runtime.transcript(&id).await.unwrap().queued[0].id,
        "follow-up-2"
    );

    harness.tools.tool_released.notify_one();
    let transcript = harness.settle(&id).await;
    assert!(transcript.queued.is_empty());
    assert_eq!(
        shape(&transcript),
        vec![
            "user: current question",
            "assistant: current answer",
            "user: answer after completion",
            "assistant: follow-up answer",
        ]
    );
    assert_eq!(harness.llm.requests_for(&id).len(), 3);
}

#[tokio::test]
async fn reject_busy_returns_error_without_claiming_the_request_id() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let gate = harness.llm.hold_stream(&id, 1);
    harness.llm.script(&id, text_reply("first answer"));
    harness.submit(&id, "first question").await;
    gate.entered.notified().await;
    assert!(matches!(
        harness
            .runtime
            .submit_request_with_mode(
                &id,
                "reject-retry",
                "retry after idle".into(),
                SubmitMode::Reject,
            )
            .await,
        Err(roost_agent::AgentError::FailedPrecondition(_))
    ));
    gate.released.notify_one();
    harness.settle(&id).await;

    harness.llm.script(&id, text_reply("retry answer"));
    harness
        .runtime
        .submit_request_with_mode(
            &id,
            "reject-retry",
            "retry after idle".into(),
            SubmitMode::Steer,
        )
        .await
        .unwrap();
    let transcript = harness.settle(&id).await;
    assert_eq!(
        shape(&transcript),
        vec![
            "user: first question",
            "assistant: first answer",
            "user: retry after idle",
            "assistant: retry answer",
        ]
    );
}

#[tokio::test]
async fn pending_steer_can_be_withdrawn_before_a_model_round_consumes_it() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let gate = harness.llm.hold_stream(&id, 1);
    harness.llm.script(&id, text_reply("first answer"));
    harness.submit(&id, "first question").await;
    gate.entered.notified().await;
    harness
        .runtime
        .submit_request(&id, "queued-steer-1", "cancel this".into())
        .await
        .unwrap();
    let mut queued_snapshot = harness.runtime.transcript(&id).await.unwrap();
    assert_eq!(queued_snapshot.queued.len(), 1);
    assert!(queued_snapshot.queued[0].mode.is_empty());
    assert!(
        harness
            .runtime
            .withdraw(&id, "withdraw-steer-1", "queued-steer-1")
            .await
            .unwrap()
    );
    let removal = harness
        .sink
        .events_for(&id)
        .into_iter()
        .find(|event| matches!(event, ChatEvent::QueueRemoved { submission_id } if submission_id == "queued-steer-1"))
        .expect("withdrawal publishes a queue removal");
    fold_chat_event(&mut queued_snapshot, &removal);
    assert!(queued_snapshot.queued.is_empty());
    gate.released.notify_one();
    let transcript = harness.settle(&id).await;
    assert_eq!(
        shape(&transcript),
        vec!["user: first question", "assistant: first answer"]
    );
    assert!(transcript.queued.is_empty());
    assert_eq!(transcript.run_state, AgentRunState::Idle);
    assert_eq!(harness.llm.requests_for(&id).len(), 1);
}

#[tokio::test]
async fn dispatcher_consumption_refuses_withdrawal_and_preserves_the_user_entry() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let gate = harness.llm.hold_stream(&id, 1);
    harness.llm.script(&id, text_reply("answer"));
    harness
        .runtime
        .submit_request_with_mode(
            &id,
            "consumed-input",
            "keep this input".into(),
            SubmitMode::Steer,
        )
        .await
        .unwrap();
    gate.entered.notified().await;

    assert!(
        !harness
            .runtime
            .withdraw(&id, "withdraw-consumed", "consumed-input")
            .await
            .unwrap()
    );
    gate.released.notify_one();
    let transcript = harness.settle(&id).await;
    assert_eq!(
        shape(&transcript),
        vec!["user: keep this input", "assistant: answer"]
    );
}
