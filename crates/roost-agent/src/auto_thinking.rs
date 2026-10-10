//! Ported from oh-my-pi packages/coding-agent/src/auto-thinking/classifier.ts (MIT).
//! The `auto` thinking level: one judge choice over how open-ended the latest
//! request is, mapped to a level. Any failure falls back to `medium`.

use roost_llm::{Answer, ModelInfo, Question};
use serde_json::json;

use crate::judging;
use crate::runtime::AgentRuntime;

const FALLBACK: &str = "medium";

const INSTRUCTIONS: &str = "The state is a user's request to a coding agent. Judge how open-ended its problem is: whether the fix or design is given, or which causes or designs remain open. Choose the reasoning effort that needs, judging inherent difficulty rather than phrasing politeness or verbosity. Volume of work never raises it. If torn between levels, choose the lower one.";

const CRITERIA: [(&str, &str); 4] = [
    (
        "low",
        "One obvious solution, mechanically applied: target, mapping, or fix given.",
    ),
    (
        "medium",
        "A few candidates in a localized area, or one small trap: which line breaks a test, one boundary case.",
    ),
    (
        "high",
        "Several viable designs or candidate causes: API shape, policy choice, a known cause whose fix needs a design choice.",
    ),
    (
        "xhigh",
        "Open cause of flaky, concurrent, or stale behavior; solutions that are easy to get subtly wrong (races, invariants, cross-version compatibility).",
    ),
];

/// The thinking level for `request`, unclamped.
pub(crate) async fn classify(
    runtime: &AgentRuntime,
    conversation_id: &str,
    model: &ModelInfo,
    request: &str,
) -> &'static str {
    let question = Question::choice("level", INSTRUCTIONS, &CRITERIA);
    let level = match judging::judge(
        runtime,
        Some(model),
        json!({"request": request}),
        vec![question],
    )
    .await
    {
        Ok(answers) => match answers.get("level") {
            Some(Answer::Choice { choice, .. }) => CRITERIA
                .iter()
                .map(|(level, _)| *level)
                .find(|level| level == choice)
                .unwrap_or(FALLBACK),
            _ => FALLBACK,
        },
        Err(error) => {
            tracing::debug!(conversation_id, %error, "auto_thinking classification failed; using medium");
            FALLBACK
        }
    };
    tracing::info!(conversation_id, level, "auto_thinking resolved");
    level
}
