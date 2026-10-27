//! Classifiers are valid judge models, not conversation models.
//! The model-scope suite verifies a rejected pin cannot replace a working chat pin.
//! Catalog metadata and conversation state come from the runtime fixture.

use super::{Harness, SONNET, pin};
use roost_agent::AgentError;
use roost_llm::ModelKind;
use roost_protocol::wire::agent_chat::ModelRef;

#[tokio::test]
async fn a_classifier_cannot_replace_a_conversations_chat_model() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.runtime.set_model(&id, pin(SONNET)).await.unwrap();
    let catalog = harness.runtime.catalog();
    let classifier = catalog
        .models()
        .iter()
        .find(|model| model.kind == ModelKind::Classifier)
        .unwrap();
    let result = harness
        .runtime
        .set_model(
            &id,
            ModelRef {
                provider: classifier.provider.clone(),
                model_id: classifier.id.clone(),
            },
        )
        .await;
    assert!(matches!(result, Err(AgentError::InvalidArgument(_))));
    assert_eq!(
        harness.runtime.conversation(&id).await.unwrap().model,
        Some(pin(SONNET))
    );
}
