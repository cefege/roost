//! A message sent while the agent is still writing a reply waits in the
//! queue and is read after that reply: the live transcript, the journal's
//! transcript and the next model request all put it there.

use super::support::{Harness, text_reply};
use roost_llm::Message;
use roost_protocol::wire::agent_chat::{ChatEvent, Transcript, TranscriptItem, fold_chat_event};

fn shape(transcript: &Transcript) -> Vec<String> {
    transcript
        .items
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::User { text, .. } => Some(format!("user: {text}")),
            TranscriptItem::Assistant { blocks, .. } => Some(format!(
                "assistant: {}",
                blocks
                    .iter()
                    .filter_map(|block| match block {
                        roost_protocol::wire::agent_chat::TranscriptBlock::Text { text } => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .collect::<String>()
            )),
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
