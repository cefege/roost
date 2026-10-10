//! Coordinator-run slash commands. Each answers with a `Notice` entry and
//! never calls the conversation's model, except `/compact`, whose summary is
//! the point. Client-only commands are refused here; the browser runs them.

use roost_protocol::wire::agent_chat::{AGENT_SLASH_COMMANDS, SlashCommand};
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::records::{ConversationRecord, model_ref};
use crate::runtime::AgentRuntime;
use crate::{advisor, compaction, plan_mode, roles, turn};

pub(crate) async fn execute(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    command: &SlashCommand,
    args: &str,
) -> Result<(), AgentError> {
    tracing::info!(conversation_id = %record.id, command = command.name, "agent slash command");
    match command.name {
        "plan" => plan_mode::toggle(runtime, record, args).await,
        "model" => set_model(runtime, record, args).await,
        "effort" => set_effort(runtime, record, args).await,
        "usage" => usage(runtime, record).await,
        "compact" => compact(runtime, record, args).await,
        "help" => {
            runtime
                .notice(&record.id, "info", "Commands", &help_table())
                .await;
            Ok(())
        }
        "advisor" => advisor::command(runtime, record, args).await,
        other => Err(AgentError::InvalidArgument(format!(
            "/{other} runs in the browser"
        ))),
    }
}

async fn set_model(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    args: &str,
) -> Result<(), AgentError> {
    if args.is_empty() {
        return Err(AgentError::InvalidArgument(
            "/model needs a selector such as anthropic/claude-sonnet-5 or @smol".into(),
        ));
    }
    let settings = runtime.settings().await?;
    let Some(resolved) =
        roles::resolve_selector(runtime.inner.llm.as_ref(), &settings, args).await?
    else {
        runtime
            .notice(
                &record.id,
                "warn",
                "Model unavailable",
                &format!("No available model matches `{args}`."),
            )
            .await;
        return Ok(());
    };
    runtime
        .set_model(
            &record.id,
            model_ref(&resolved.info.provider, &resolved.info.id),
        )
        .await?;
    if let Some(level) = resolved.thinking {
        runtime.set_thinking_level(&record.id, level).await?;
    }
    runtime
        .notice(
            &record.id,
            "info",
            "Model changed",
            &format!(
                "Now using {} (`{}/{}`).",
                resolved.info.name, resolved.info.provider, resolved.info.id
            ),
        )
        .await;
    Ok(())
}

async fn set_effort(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    args: &str,
) -> Result<(), AgentError> {
    let level = args.trim().to_ascii_lowercase();
    if level.is_empty() {
        return Err(AgentError::InvalidArgument(
            "/effort needs a level: off, minimal, low, medium, high, xhigh, max or auto".into(),
        ));
    }
    runtime
        .set_thinking_level(&record.id, level.clone())
        .await?;
    runtime
        .notice(
            &record.id,
            "info",
            "Thinking level changed",
            &format!("Thinking level: `{level}`."),
        )
        .await;
    Ok(())
}

async fn usage(runtime: &AgentRuntime, record: &ConversationRecord) -> Result<(), AgentError> {
    let rows = runtime.inner.llm.account_usage().await;
    let body = if rows.is_empty() {
        "No accounts are connected.".to_owned()
    } else {
        let mut table =
            String::from("| Provider | Account | Usage | Blocked until |\n|---|---|---|---|\n");
        for row in rows {
            let mut usage: Vec<String> = row
                .windows
                .iter()
                .map(|window| {
                    let reset = window
                        .resets_at_ms
                        .map(|at| format!(", resets {}", format_time(at)))
                        .unwrap_or_default();
                    format!(
                        "{} {:.0}%{reset}",
                        window.name,
                        window.used_fraction * 100.0
                    )
                })
                .collect();
            if let Some(note) = row.note {
                usage.push(note);
            }
            if let Some(cause) = row.disabled_cause {
                usage.push(format!("disabled: {cause}"));
            }
            let blocked = row
                .blocked_until_ms
                .filter(|until| *until > crate::runtime::now_ms())
                .map(format_time)
                .unwrap_or_else(|| "—".into());
            table.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                row.provider,
                row.label,
                usage.join("; "),
                blocked
            ));
        }
        table
    };
    runtime
        .notice(&record.id, "info", "Account usage", &body)
        .await;
    Ok(())
}

async fn compact(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    args: &str,
) -> Result<(), AgentError> {
    if runtime.is_running(&record.id) {
        return Err(AgentError::FailedPrecondition(
            "wait for the current run to finish before compacting".into(),
        ));
    }
    let model = turn::conversation_model(runtime, record).await?;
    let runtime = runtime.clone();
    let record = record.clone();
    let instructions = args.to_owned();
    tokio::spawn(async move {
        let cancel = CancellationToken::new();
        match compaction::compact(&runtime, &record, &model.info, Some(&instructions), &cancel)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                runtime
                    .notice(
                        &record.id,
                        "info",
                        "Nothing to compact",
                        "The conversation is already short.",
                    )
                    .await
            }
            Err(error) => {
                runtime
                    .notice(&record.id, "error", "Compaction failed", &error.to_string())
                    .await
            }
        }
    });
    Ok(())
}

fn help_table() -> String {
    let mut table = String::from("| Command | Arguments | What it does |\n|---|---|---|\n");
    for command in AGENT_SLASH_COMMANDS {
        let aliases = if command.aliases.is_empty() {
            String::new()
        } else {
            format!(" (also /{})", command.aliases.join(", /"))
        };
        table.push_str(&format!(
            "| /{}{aliases} | {} | {} |\n",
            command.name, command.args, command.summary
        ));
    }
    table
}

/// UTC `YYYY-MM-DD HH:MM` for a millisecond timestamp.
fn format_time(at_ms: i64) -> String {
    let seconds = at_ms.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let minute_of_day = seconds.rem_euclid(86_400) / 60;
    // Civil-from-days (Howard Hinnant), valid for the proleptic Gregorian calendar.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        minute_of_day / 60,
        minute_of_day % 60
    )
}

#[cfg(test)]
mod tests {
    use super::format_time;

    #[test]
    fn formats_utc_timestamps() {
        assert_eq!(format_time(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_time(1_709_251_200_000), "2024-03-01 00:00 UTC");
    }
}
