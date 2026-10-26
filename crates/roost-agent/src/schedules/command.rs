//! `/schedule` grammar: `in`/`every`/`at` create a row, `cancel` removes one,
//! bare lists them. Parsing and limit checks are pure (`parse_args`); the
//! async half only touches the store and emits the result.

use roost_protocol::wire::agent_chat::ChatEvent;

use super::{ScheduleRow, to_wire};
use crate::error::AgentError;
use crate::records::ConversationRecord;
use crate::runtime::{AgentRuntime, now_ms};

const MAX_TEXT_CHARS: usize = 4_000;
const MIN_EVERY_MS: i64 = 10 * 60_000;
const MAX_AHEAD_MS: i64 = 366 * 86_400_000;
const MAX_PER_CONVERSATION: usize = 50;
const USAGE: &str = "Usage: /schedule [in <duration> <text>|every <duration> <text>|at <RFC3339> <text>|cancel <id>]. Examples: /schedule in 2h check the deploy, /schedule every 1d summarize open PRs, /schedule cancel s-1a2b3c4d.";

#[derive(Debug, Clone, PartialEq)]
enum Action {
    List,
    Cancel {
        id: String,
    },
    Create {
        text: String,
        due_ms: i64,
        every_ms: Option<i64>,
    },
}

pub(crate) async fn command(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    args: &str,
) -> Result<(), AgentError> {
    match parse_args(args, now_ms())? {
        Action::List => list(runtime, record).await,
        Action::Cancel { id } => cancel(runtime, record, &id).await,
        Action::Create {
            text,
            due_ms,
            every_ms,
        } => create(runtime, record, text, due_ms, every_ms).await,
    }
}

fn parse_args(args: &str, now_ms: i64) -> Result<Action, AgentError> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Ok(Action::List);
    }
    let (verb, rest) = trimmed
        .split_once(char::is_whitespace)
        .unwrap_or((trimmed, ""));
    let rest = rest.trim_start();
    match verb {
        "cancel" => {
            let id = rest.trim();
            if id.is_empty() {
                return Err(AgentError::InvalidArgument(USAGE.to_owned()));
            }
            Ok(Action::Cancel { id: id.to_owned() })
        }
        "in" => {
            let (word, text) = split_word(rest)?;
            let text = validate_text(text)?;
            let duration_ms = duration_ms(word)?;
            let due_ms = now_ms.saturating_add(duration_ms);
            validate_ahead(due_ms, now_ms)?;
            Ok(Action::Create {
                text,
                due_ms,
                every_ms: None,
            })
        }
        "every" => {
            let (word, text) = split_word(rest)?;
            let text = validate_text(text)?;
            let every_ms = duration_ms(word)?;
            if every_ms < MIN_EVERY_MS {
                return Err(AgentError::InvalidArgument(format!(
                    "/schedule every must repeat at least every {}",
                    format_duration_ms(MIN_EVERY_MS)
                )));
            }
            let due_ms = now_ms.saturating_add(every_ms);
            validate_ahead(due_ms, now_ms)?;
            Ok(Action::Create {
                text,
                due_ms,
                every_ms: Some(every_ms),
            })
        }
        "at" => {
            let (word, text) = split_word(rest)?;
            let text = validate_text(text)?;
            let parsed = chrono::DateTime::parse_from_rfc3339(word).map_err(|error| {
                AgentError::InvalidArgument(format!("invalid RFC 3339 time: {error}"))
            })?;
            let due_ms = parsed.timestamp_millis();
            validate_ahead(due_ms, now_ms)?;
            Ok(Action::Create {
                text,
                due_ms,
                every_ms: None,
            })
        }
        _ => Err(AgentError::InvalidArgument(USAGE.to_owned())),
    }
}

fn split_word(rest: &str) -> Result<(&str, &str), AgentError> {
    rest.split_once(char::is_whitespace)
        .map(|(word, text)| (word, text.trim_start()))
        .ok_or_else(|| AgentError::InvalidArgument(USAGE.to_owned()))
}

fn duration_ms(word: &str) -> Result<i64, AgentError> {
    let duration =
        crate::loop_mode::parse::parse_duration(word).map_err(AgentError::InvalidArgument)?;
    Ok(i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
}

fn validate_text(text: &str) -> Result<String, AgentError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(AgentError::InvalidArgument(
            "scheduled message text is required".into(),
        ));
    }
    if trimmed.chars().count() > MAX_TEXT_CHARS {
        return Err(AgentError::InvalidArgument(format!(
            "scheduled message text must be at most {MAX_TEXT_CHARS} characters"
        )));
    }
    Ok(trimmed.to_owned())
}

fn validate_ahead(due_ms: i64, now_ms: i64) -> Result<(), AgentError> {
    if due_ms <= now_ms {
        return Err(AgentError::InvalidArgument(
            "scheduled time must be in the future".into(),
        ));
    }
    if due_ms - now_ms > MAX_AHEAD_MS {
        return Err(AgentError::InvalidArgument(format!(
            "scheduled time must be within {} days",
            MAX_AHEAD_MS / 86_400_000
        )));
    }
    Ok(())
}

fn check_capacity(count: usize) -> Result<(), AgentError> {
    if count >= MAX_PER_CONVERSATION {
        return Err(AgentError::InvalidArgument(format!(
            "at most {MAX_PER_CONVERSATION} scheduled messages per conversation"
        )));
    }
    Ok(())
}

async fn create(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    text: String,
    due_ms: i64,
    every_ms: Option<i64>,
) -> Result<(), AgentError> {
    let existing = runtime.inner.store.schedules(&record.id).await?;
    check_capacity(existing.len())?;
    let id = mint_schedule_id(&existing)?;
    let row = ScheduleRow {
        id: id.clone(),
        text,
        due_ms,
        every_ms,
        fired: 0,
        created_ms: now_ms(),
    };
    runtime.inner.store.set_schedule(&record.id, &row).await?;
    runtime
        .emit(
            &record.id,
            vec![ChatEvent::ScheduleSet {
                schedule: to_wire(&row),
            }],
        )
        .await;
    runtime
        .notice(
            &record.id,
            "info",
            "Scheduled",
            &format!(
                "`{id}` · {} · {} · {}",
                describe_kind(every_ms),
                describe_due(due_ms, now_ms()),
                row.text
            ),
        )
        .await;
    Ok(())
}

async fn cancel(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    id: &str,
) -> Result<(), AgentError> {
    let removed = runtime.inner.store.remove_schedule(&record.id, id).await?;
    if !removed {
        return Err(AgentError::InvalidArgument(format!("no schedule {id}")));
    }
    runtime
        .emit(
            &record.id,
            vec![ChatEvent::ScheduleRemoved { id: id.to_owned() }],
        )
        .await;
    runtime
        .notice(
            &record.id,
            "info",
            "Schedule cancelled",
            &format!("`{id}` will not run."),
        )
        .await;
    Ok(())
}

async fn list(runtime: &AgentRuntime, record: &ConversationRecord) -> Result<(), AgentError> {
    let rows = runtime.inner.store.schedules(&record.id).await?;
    if rows.is_empty() {
        runtime
            .notice(
                &record.id,
                "info",
                "Scheduled messages",
                "Nothing scheduled.",
            )
            .await;
        return Ok(());
    }
    let now = now_ms();
    let mut body = String::new();
    for row in &rows {
        let text: String = row.text.chars().take(80).collect();
        body.push_str(&format!(
            "- `{}` · {} · {} · {text}\n",
            row.id,
            describe_due(row.due_ms, now),
            describe_kind(row.every_ms),
        ));
    }
    runtime
        .notice(&record.id, "info", "Scheduled messages", body.trim_end())
        .await;
    Ok(())
}

fn describe_kind(every_ms: Option<i64>) -> String {
    match every_ms {
        None => "once".to_owned(),
        Some(step) => format!("every {}", format_duration_ms(step)),
    }
}

fn describe_due(due_ms: i64, now_ms: i64) -> String {
    let remaining = due_ms - now_ms;
    let at = crate::commands::format_time(due_ms);
    if remaining <= 0 {
        format!("due ({at})")
    } else {
        format!("in {} ({at})", format_duration_ms(remaining))
    }
}

fn format_duration_ms(ms: i64) -> String {
    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 86_400_000;
    let ms = ms.max(0);
    if ms >= DAY {
        format!("{}d", ms / DAY)
    } else if ms >= HOUR {
        format!("{}h", ms / HOUR)
    } else if ms >= MINUTE {
        format!("{}m", ms / MINUTE)
    } else {
        format!("{}s", (ms / 1_000).max(1))
    }
}

/// `s-` plus 4 random bytes: short enough that a collision within one
/// conversation's schedules is a real (if rare) event, so creation retries
/// the mint once before giving up.
fn mint_schedule_id(existing: &[ScheduleRow]) -> Result<String, AgentError> {
    for _ in 0..2 {
        let candidate = random_short_id();
        if !existing.iter().any(|row| row.id == candidate) {
            return Ok(candidate);
        }
    }
    Err(AgentError::Store(
        "could not mint a unique schedule id".into(),
    ))
}

fn random_short_id() -> String {
    let mut bytes = [0_u8; 4];
    if getrandom::fill(&mut bytes).is_err() {
        bytes.copy_from_slice(&now_ms().to_le_bytes()[..4]);
    }
    format!("s-{}", hex::encode(bytes))
}

#[cfg(test)]
#[path = "command/tests.rs"]
mod tests;
