//! Rewind and retry must replace the transcript already visible to subscribers.
//! The recorded runtime event stream is folded through the shared client contract.
//! Retained turns survive; a hidden turn cannot remain beside its replacement.

use roost_protocol::wire::agent_chat::{
    Transcript, TranscriptBlock, TranscriptItem, fold_chat_event,
};

use super::support::{Harness, text_reply};

fn visible_turns(
    harness: &Harness,
    conversation_id: &str,
    mut transcript: Transcript,
) -> Vec<String> {
    for event in harness.sink.events_for(conversation_id) {
        fold_chat_event(&mut transcript, &event);
    }
    transcript
        .items
        .into_iter()
        .filter_map(|item| match item {
            TranscriptItem::User { text, .. } => Some(text),
            TranscriptItem::Assistant { blocks, .. } => Some(
                blocks
                    .into_iter()
                    .filter_map(|block| match block {
                        TranscriptBlock::Text { text } => Some(text),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            ),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_live_subscriber_drops_the_hidden_turn_before_its_replacement() {
    let harness = Harness::new();
    let conversation_id = harness.conversation().await;
    let initial = harness.runtime.transcript(&conversation_id).await.unwrap();
    for reply in ["kept answer", "discarded answer", "replacement answer"] {
        harness.llm.script(&conversation_id, text_reply(reply));
    }
    harness.submit(&conversation_id, "keep this turn").await;
    harness.settle(&conversation_id).await;
    harness.submit(&conversation_id, "replace this turn").await;
    harness.settle(&conversation_id).await;
    let target_id = harness
        .runtime
        .transcript(&conversation_id)
        .await
        .unwrap()
        .items
        .into_iter()
        .find_map(|item| match item {
            TranscriptItem::User { id, text, .. } if text == "replace this turn" => Some(id),
            _ => None,
        })
        .unwrap();

    harness
        .runtime
        .rewind(&conversation_id, &target_id, false)
        .await
        .unwrap();
    assert_eq!(
        visible_turns(&harness, &conversation_id, initial.clone()),
        ["keep this turn", "kept answer"]
    );

    harness.submit(&conversation_id, "replacement turn").await;
    harness.settle(&conversation_id).await;
    assert_eq!(
        visible_turns(&harness, &conversation_id, initial),
        [
            "keep this turn",
            "kept answer",
            "replacement turn",
            "replacement answer"
        ]
    );
}
