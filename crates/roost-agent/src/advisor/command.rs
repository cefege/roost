//! Implements `/advisor` toggling and the current review, model, token, and
//! estimated-cost status without invoking a model.

use crate::error::AgentError;
use crate::records::ConversationRecord;
use crate::runtime::AgentRuntime;

use super::hub::AdvisorHub;

pub(crate) async fn command(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    args: &str,
) -> Result<(), AgentError> {
    let args = args.trim();
    match args {
        "status" => show_status(runtime, record).await,
        "on" => set_enabled(runtime, record, true).await,
        "off" => set_enabled(runtime, record, false).await,
        "" => {
            let enabled = record
                .advisor
                .unwrap_or(runtime.inner.store.settings().await?.advisor_enabled);
            set_enabled(runtime, record, !enabled).await
        }
        other => {
            runtime.notice(&record.id, "info", "Advisor command", &format!("Unknown argument `{other}`. Use `/advisor`, `/advisor on`, `/advisor off`, or `/advisor status`. ")).await;
            Ok(())
        }
    }
}

async fn set_enabled(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    enabled: bool,
) -> Result<(), AgentError> {
    runtime
        .update_record(&record.id, |row| row.advisor = Some(enabled))
        .await?;
    AdvisorHub::reset(runtime, &record.id);
    runtime
        .notice(
            &record.id,
            "info",
            if enabled {
                "Advisor enabled"
            } else {
                "Advisor disabled"
            },
            "",
        )
        .await;
    Ok(())
}

async fn show_status(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
) -> Result<(), AgentError> {
    let status = AdvisorHub::status(runtime, record).await;
    let model = runtime
        .catalog()
        .models()
        .iter()
        .find(|model| format!("{}/{}", model.provider, model.id) == status.model);
    let cost = model.map_or(0.0, |model| {
        status.input_tokens as f64 * model.cost.input / 1_000_000.0
            + status.output_tokens as f64 * model.cost.output / 1_000_000.0
    });
    runtime.notice(&record.id, "info", "Advisor status", &format!(
        "state: {}; model: {}; reviews: {}; notes accepted: {}; tokens: {} input / {} output; estimated cost: ${cost:.6}",
        status.state, status.model, status.reviews, status.accepted, status.input_tokens, status.output_tokens
    )).await;
    Ok(())
}
