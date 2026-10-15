//! `/model <x>` and `/switch <x>`: exact selectors still win, and a bare name
//! picks the best fuzzy match among models the account pool can reach.

#[path = "support/mod.rs"]
mod support;

use roost_protocol::wire::agent_chat::{ModelRef, TranscriptItem};
use support::Harness;

async fn model_of(harness: &Harness, id: &str) -> (Option<ModelRef>, Option<String>) {
    let conversation = harness
        .runtime
        .conversations()
        .await
        .unwrap()
        .into_iter()
        .find(|conversation| conversation.id == id)
        .unwrap();
    (conversation.model, conversation.thinking_level)
}

fn model(id: &str) -> Option<ModelRef> {
    Some(ModelRef {
        provider: "anthropic".into(),
        model_id: id.into(),
    })
}

#[tokio::test]
async fn a_bare_name_switches_to_the_newest_fuzzy_match() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.submit(&id, "/switch sonnet").await;
    assert_eq!(model_of(&harness, &id).await.0, model("claude-sonnet-5-5"));
    harness.submit(&id, "/switch haiku:low").await;
    assert_eq!(
        model_of(&harness, &id).await,
        (model("claude-haiku-5-5"), Some("low".into()))
    );
    harness.submit(&id, "/model sonnet 5").await;
    assert_eq!(model_of(&harness, &id).await.0, model("claude-sonnet-5-5"));
    harness.submit(&id, "/model anthropic/claude-opus-5").await;
    assert_eq!(model_of(&harness, &id).await.0, model("claude-opus-5"));
}

#[tokio::test]
async fn a_name_without_a_reachable_model_leaves_the_chat_unchanged() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let before = model_of(&harness, &id).await;
    harness.submit(&id, "/switch gpt").await;
    let transcript = harness.settle(&id).await;
    assert!(
        transcript.items.iter().any(|item| matches!(item,
        TranscriptItem::Notice { body, .. } if body.contains("No available model matches `gpt`.")))
    );
    assert_eq!(model_of(&harness, &id).await, before);
    assert!(
        harness
            .runtime
            .submit(&id, "/switch @nope".into())
            .await
            .is_err()
    );
}
