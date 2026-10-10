//! Ported from oh-my-pi packages/coding-agent/src/session/unexpected-stop-classifier.ts (MIT).
//! After a text-only answer, one yes/no judgment: did the assistant say it
//! would act and then stop? No judge, or a judge failure, means "no".

use roost_llm::{Answer, AssistantBlock, ModelInfo, Question, StopReason};
use serde_json::json;

use crate::judging;
use crate::model_call::ModelOutput;
use crate::runtime::AgentRuntime;

/// A coin-flip message must not trigger a retry.
const THRESHOLD: f64 = 0.5;

const INSTRUCTIONS: &str = "Classify whether this assistant message is an unexpected stop: it says it will act, continue working, or call a tool, then ends without doing so.";
const WHEN_TRUE: &str = "Unexpected stops:\n- \"I should do the same for the JS eval worker. Doing that now.\"\n- \"Let me run the tests next.\"\n- \"I'll fix that now.\"\n- \"Should I do that for you?\"";
const WHEN_FALSE: &str = "Not an unexpected stop:\n- \"I've completed the task.\"\n- \"Is there anything else I can help with?\"\n- \"The fix is done and tests pass.\"";

/// Whether a text-only turn is a candidate at all: a normal stop with text.
fn is_candidate(output: &ModelOutput) -> bool {
    output.stop == StopReason::EndTurn
        && output
            .blocks
            .iter()
            .any(|block| matches!(block, AssistantBlock::Text { text } if !text.trim().is_empty()))
        && !output
            .blocks
            .iter()
            .any(|block| matches!(block, AssistantBlock::ToolCall { .. }))
}

pub(crate) async fn stopped_unexpectedly(
    runtime: &AgentRuntime,
    conversation_id: &str,
    model: &ModelInfo,
    output: &ModelOutput,
) -> bool {
    if !is_candidate(output) {
        return false;
    }
    let question = Question::boolean("stopped", INSTRUCTIONS, WHEN_TRUE, WHEN_FALSE);
    match judging::judge(
        runtime,
        Some(model),
        json!({"message": output.text()}),
        vec![question],
    )
    .await
    {
        Ok(answers) => {
            matches!(answers.get("stopped"), Some(Answer::Bool { probability }) if *probability >= THRESHOLD)
        }
        Err(error) => {
            tracing::debug!(conversation_id, %error, "unexpected-stop classification failed; skipping");
            false
        }
    }
}
