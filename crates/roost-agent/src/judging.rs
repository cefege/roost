//! Ported from oh-my-pi packages/coding-agent/src/judgment/index.ts (MIT).
//! Judgments through the `judge` role chain: each candidate in order until
//! one answers. Callers treat a failure as "no judgment" and carry on.

use std::collections::BTreeMap;

use roost_llm::{Answer, ModelInfo, Question};

use crate::error::AgentError;
use crate::roles;
use crate::runtime::AgentRuntime;

/// Asks `questions` about `state`; `session_model` closes a chain with no
/// native candidate.
pub(crate) async fn judge(
    runtime: &AgentRuntime,
    session_model: Option<&ModelInfo>,
    state: serde_json::Value,
    questions: Vec<Question>,
) -> Result<BTreeMap<String, Answer>, AgentError> {
    let settings = runtime.inner.store.settings().await?;
    let llm = runtime.inner.llm.as_ref();
    let candidates = roles::judge_candidates(llm, &settings, session_model).await;
    let mut last_error = AgentError::FailedPrecondition("no judge model is available".into());
    for candidate in &candidates {
        match llm.judge(candidate, state.clone(), questions.clone()).await {
            Ok(answers) => return Ok(answers),
            Err(error) => {
                tracing::debug!(provider = %candidate.provider, model = %candidate.id, %error, "judge candidate failed");
                last_error = error.into();
            }
        }
    }
    Err(last_error)
}

/// Whether the judge chain starts with a native (System One) model.
pub(crate) async fn has_native_judge(runtime: &AgentRuntime) -> bool {
    let Ok(settings) = runtime.inner.store.settings().await else {
        return false;
    };
    roles::judge_candidates(runtime.inner.llm.as_ref(), &settings, None)
        .await
        .first()
        .is_some_and(roles::is_native_judge)
}

/// Whether any judge resolves (so `auto` thinking is offered).
pub(crate) async fn has_judge(runtime: &AgentRuntime) -> bool {
    let Ok(settings) = runtime.inner.store.settings().await else {
        return false;
    };
    !roles::judge_candidates(runtime.inner.llm.as_ref(), &settings, None)
        .await
        .is_empty()
}
