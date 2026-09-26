//! `roost doctor --session <sid>`: one session's events in time order, merged
//! from every source. Called by doctor/mod.rs. Answers "is this session alive,
//! when did it last echo, where did input stop" without log spelunking, which is
//! the question a report of a stuck terminal always comes down to.
//!
//! It reads `main.out.log` as well as `main.err.log`, which the digest
//! deliberately does not: the firehose is what carries per-session bytes, cells
//! and focus events, and it is only switched on when somebody is reproducing a
//! problem. Signals show either way, which is why the digest is the default.

use serde_json::Value;

use crate::doctor::digest::{number_field, short, string_field};
use crate::doctor::log_sources::{LogSource, for_each_log_line, log_files_for};
use crate::utc_clock::format_utc_millisecond_time;

/// Keys that are already a column, or that are this row's own identity, so they
/// must not be repeated in the key/value tail. Everything else is context, and
/// the first few of them are usually the answer.
const STRUCTURAL_KEYS: [&str; 10] = [
    "ts",
    "level",
    "target",
    "evt",
    "msg",
    "sid",
    "session_id",
    "sessionId",
    "mono_ns",
    "session_trace_id",
];

/// How many key/value pairs ride along on a row. More than this is a dump, and
/// a dump of forty columns answers "where did input stop" worse than none.
pub const CONTEXT_FIELDS: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineRow {
    pub ts_ms: i64,
    /// The writer's monotonic counter. Two events in the same millisecond are
    /// ordered by it, which is the only thing that distinguishes a PTY byte
    /// from the resize that preceded it.
    pub mono_ns: i64,
    pub app: String,
    pub channel: String,
    pub label: String,
}

/// Does this line belong to `sid`? A prefix matches, because an operator reads
/// an id off a URL and pastes the first segment of it.
fn belongs_to(line: &Value, sid: &str) -> bool {
    ["sid", "session_id", "sessionId"]
        .iter()
        .filter_map(|key| string_field(line, key))
        .any(|candidate| candidate == sid || candidate.starts_with(sid))
}

pub fn collect(sources: &[LogSource], sid: &str, cutoff_ms: i64) -> Vec<TimelineRow> {
    let mut rows: Vec<TimelineRow> = Vec::new();
    for source in sources {
        for base in ["main.err.log", "main.out.log"] {
            let mut candidate = source.clone();
            candidate.base = base.to_string();
            for file in log_files_for(&candidate) {
                for_each_log_line(&file, &mut |line| {
                    let ts_ms = number_field(&line, "ts").unwrap_or(0);
                    if ts_ms < cutoff_ms || !belongs_to(&line, sid) {
                        return;
                    }
                    rows.push(TimelineRow {
                        ts_ms,
                        mono_ns: number_field(&line, "mono_ns").unwrap_or(0),
                        app: source.app.clone(),
                        channel: channel_of(&line),
                        label: label_of(&line),
                    });
                });
            }
        }
    }
    rows.sort_by(|left, right| {
        left.ts_ms
            .cmp(&right.ts_ms)
            .then(left.mono_ns.cmp(&right.mono_ns))
    });
    rows
}

fn channel_of(line: &Value) -> String {
    if string_field(line, "target") == Some("signal") {
        return "SIGNAL".to_string();
    }
    string_field(line, "target")
        .or_else(|| string_field(line, "level"))
        .unwrap_or("log")
        .to_string()
}

fn label_of(line: &Value) -> String {
    let event = string_field(line, "evt")
        .or_else(|| string_field(line, "msg"))
        .unwrap_or("?");
    let context = context_fields(line);
    if context.is_empty() {
        return event.to_string();
    }
    format!("{event} {context}")
}

fn context_fields(line: &Value) -> String {
    let Some(object) = line.as_object() else {
        return String::new();
    };
    let mut rendered: Vec<String> = Vec::new();
    for (key, value) in object {
        if STRUCTURAL_KEYS.contains(&key.as_str()) {
            continue;
        }
        rendered.push(format!("{key}={}", short(&scalar_text(value), 24)));
        if rendered.len() == CONTEXT_FIELDS {
            break;
        }
    }
    rendered.join(" ")
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The timeline, as one string. An empty result prints WHY it is empty, because
/// "no events" from a gated firehose reads as "nothing happened" and sends the
/// next hour of the investigation in the wrong direction.
pub fn render(sid: &str, rows: &[TimelineRow]) -> String {
    let mut lines = vec![format!(
        "# roost doctor --session {sid}  ({} events)",
        rows.len()
    )];
    if rows.is_empty() {
        lines.push(
            "  no events. The diag firehose is gated — set ROOST_DIAG=1 (worker/coord) +"
                .to_string(),
        );
        lines.push(
            "  localStorage.roostDiag='1' (SPA) and reproduce; signals show without it."
                .to_string(),
        );
        return lines.join("\n");
    }
    for row in rows {
        lines.push(format!(
            "  {} {:<6} {:<7} {}",
            format_utc_millisecond_time(row.ts_ms),
            row.app,
            row.channel,
            row.label
        ));
    }
    lines.join("\n")
}
