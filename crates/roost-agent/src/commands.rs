//! Coordinator-run slash commands. Each answers with a `Notice` entry; `/btw`
//! calls a side model and `/compact` summarizes history. Client-only commands
//! are refused here; the browser runs them.

use roost_protocol::wire::agent_chat::{AGENT_SLASH_COMMANDS, SlashCommand};

use crate::error::AgentError;
use crate::journal::effective_entries;
use crate::records::{ConversationRecord, Entry, model_ref};
use crate::runtime::AgentRuntime;
use crate::{advisor, btw, model_presets, model_switch, plan_mode};

pub(crate) async fn execute(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    command: &SlashCommand,
    args: &str,
) -> Result<(), AgentError> {
    tracing::info!(conversation_id = %record.id, command = command.name, "agent slash command");
    match command.name {
        "plan" => plan_mode::toggle(runtime, record, args).await,
        "model" | "switch" => set_model(runtime, record, command.name, args).await,
        "modelpreset" => model_presets::command(runtime, record, args).await,
        "effort" => set_effort(runtime, record, args).await,
        "usage" => usage(runtime, record).await,
        "compact" => compact(runtime, record, args).await,
        "btw" => btw::run(runtime, &record.id, args).await,
        "rename" => rename(runtime, record, args).await,
        "retry" => retry(runtime, record).await,
        "rewind" => rewind_latest(runtime, record).await,
        "fork" => fork(runtime, record).await,
        "help" => {
            runtime
                .notice(&record.id, "info", "Commands", &help_table())
                .await;
            Ok(())
        }
        "goal" => runtime.goal_command(record, args).await,
        "advisor" => advisor::command(runtime, record, args).await,
        "loop" => runtime.run_loop_command(&record.id, args).await,
        "schedule" => crate::schedules::command(runtime, record, args).await,
        other => Err(AgentError::InvalidArgument(format!(
            "/{other} runs in the browser"
        ))),
    }
}
async fn retry(runtime: &AgentRuntime, record: &ConversationRecord) -> Result<(), AgentError> {
    let (item_id, _) = latest_user(runtime, &record.id).await?;
    let text = runtime.rewind(&record.id, &item_id, false).await?;
    runtime
        .notice(
            &record.id,
            "info",
            "Retrying turn",
            "Resubmitting the previous user message.",
        )
        .await;
    Box::pin(runtime.submit(&record.id, text)).await
}

async fn rewind_latest(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
) -> Result<(), AgentError> {
    let (item_id, _) = latest_user(runtime, &record.id).await?;
    runtime.rewind(&record.id, &item_id, true).await?;
    Ok(())
}

async fn fork(runtime: &AgentRuntime, record: &ConversationRecord) -> Result<(), AgentError> {
    let fork = runtime.fork(&record.id, None).await?;
    runtime
        .notice(
            &record.id,
            "info",
            "Conversation forked",
            &format!("New conversation: [Open fork](/?conversation={})", fork.id),
        )
        .await;
    Ok(())
}

async fn latest_user(
    runtime: &AgentRuntime,
    conversation_id: &str,
) -> Result<(String, String), AgentError> {
    effective_entries(&runtime.inner.store.entries(conversation_id).await?)
        .into_iter()
        .rev()
        .find_map(|(seq, entry)| match entry {
            Entry::User { text, item_id, .. } => Some((
                item_id.unwrap_or_else(|| crate::projection::user_item_id(seq)),
                text,
            )),
            _ => None,
        })
        .ok_or_else(|| AgentError::InvalidArgument("no user turn to rewind".into()))
}

async fn set_model(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    name: &str,
    args: &str,
) -> Result<(), AgentError> {
    if args.is_empty() {
        return Err(AgentError::InvalidArgument(format!(
            "/{name} needs a model: a name like sonnet, a provider/id such as anthropic/claude-sonnet-5, or a role such as @smol"
        )));
    }
    let settings = runtime.settings_for(record).await?;
    let Some(resolved) =
        model_switch::resolve_session_selector(runtime.inner.llm.as_ref(), &settings, args).await?
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
    let runtime = runtime.clone();
    let record = record.clone();
    let instructions = args.to_owned();
    tokio::spawn(async move {
        match runtime
            .compact_durable(&record.id, Some(instructions))
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

async fn rename(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    args: &str,
) -> Result<(), AgentError> {
    let title = args.trim();
    if title.is_empty() {
        return Err(AgentError::InvalidArgument("/rename needs a title".into()));
    }
    runtime
        .set_details(&record.id, Some(title.to_owned()), None, None)
        .await?;
    runtime
        .notice(&record.id, "info", "Conversation renamed", title)
        .await;
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
pub(crate) fn format_time(at_ms: i64) -> String {
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
