//! Ported from oh-my-pi packages/coding-agent/src/judgment/index.ts (MIT).
//! Judgments through the `judge` role chain: each candidate in order until
//! one answers. Callers treat a failure as "no judgment" and carry on.

use std::collections::BTreeMap;

use roost_llm::{Answer, ModelInfo, Question};

use crate::error::AgentError;
use crate::model_call::falls_over;
use crate::records::AgentSettings;
use crate::roles;
use crate::runtime::AgentRuntime;

/// Asks `questions` about `state` through the conversation's judge chain;
/// `session_model` closes a chain with no native candidate.
pub(crate) async fn judge(
    runtime: &AgentRuntime,
    conversation_id: &str,
    session_model: Option<&ModelInfo>,
    state: serde_json::Value,
    questions: Vec<Question>,
) -> Result<BTreeMap<String, Answer>, AgentError> {
    let settings = conversation_settings(runtime, conversation_id).await?;
    let llm = runtime.inner.llm.as_ref();
    let candidates = roles::judge_candidates(llm, &settings, session_model).await;
    let mut last_error = AgentError::FailedPrecondition("no judge model is available".into());
    for candidate in &candidates {
        match llm.judge(candidate, state.clone(), questions.clone()).await {
            Ok(answers) => return Ok(answers),
            Err(error) if falls_over(&error) => {
                tracing::debug!(
                    provider = %candidate.provider,
                    model = %candidate.id,
                    %error,
                    "judge candidate failed"
                );
                last_error = error.into();
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(last_error)
}

/// Whether the conversation's judge chain starts with a native (System One) model.
pub(crate) async fn has_native_judge(runtime: &AgentRuntime, conversation_id: &str) -> bool {
    let Ok(settings) = conversation_settings(runtime, conversation_id).await else {
        return false;
    };
    roles::judge_candidates(runtime.inner.llm.as_ref(), &settings, None)
        .await
        .first()
        .is_some_and(roles::is_native_judge)
}

/// Whether any judge resolves globally or in a folder override, so `auto`
/// thinking is offered wherever a folder's judge can classify.
pub(crate) async fn has_judge(runtime: &AgentRuntime) -> bool {
    let Ok(settings) = runtime.inner.store.settings().await else {
        return false;
    };
    let llm = runtime.inner.llm.as_ref();
    if !roles::judge_candidates(llm, &settings, None)
        .await
        .is_empty()
    {
        return true;
    }
    for row in &settings.folder_model_roles {
        let folder = settings.for_folder(&row.worker_fp, &row.cwd);
        if !roles::judge_candidates(llm, &folder, None).await.is_empty() {
            return true;
        }
    }
    false
}

/// The judge chain follows the conversation's folder overrides, so it is
/// resolved from the record rather than the global document.
async fn conversation_settings(
    runtime: &AgentRuntime,
    conversation_id: &str,
) -> Result<AgentSettings, AgentError> {
    let record = runtime.record(conversation_id).await?;
    runtime.settings_for(&record).await
}
