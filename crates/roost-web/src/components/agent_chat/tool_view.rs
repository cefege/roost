//! What one tool call shows, as pure functions: its verb and target, the
//! slice of output visible while folded (OMP's per-tool preview windows), and
//! the executor facts. The component is `tool_card`.

use std::collections::BTreeSet;

use crate::components::ui::IconType;

/// Which end of the output a folded preview keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreviewEdge {
    /// The latest lines, for output that grows (commands, subagents).
    Tail,
    /// The first lines, for results that are lists (reads, searches).
    Head,
}

/// Rows a folded tool shows, per OMP's renderers: bash keeps the last 10,
/// a read its first 3, grep 6, glob 8, find and fetch 12, a subagent 3, an
/// edit's diff 12 and a write 6.
pub(super) fn preview_window(tool_name: &str) -> (usize, PreviewEdge) {
    match tool_name {
        "read" => (3, PreviewEdge::Head),
        "grep" => (6, PreviewEdge::Head),
        "write" => (6, PreviewEdge::Head),
        "glob" | "ls" | "web_search" => (8, PreviewEdge::Head),
        "find" | "fetch" | "edit" => (12, PreviewEdge::Head),
        "task" => (3, PreviewEdge::Tail),
        _ => (10, PreviewEdge::Tail),
    }
}

/// An error keeps at most this many rows folded, as OMP's transcript does.
pub(super) const ERROR_ROWS: usize = 8;

/// The Lucide icon and verb for a call in its current state.
pub(super) fn presentation(tool_name: &str, active: bool) -> (IconType, &str) {
    let (icon, running_verb, finished_verb) = match tool_name {
        "bash" => (IconType::Terminal, "Running", "Ran"),
        "read" => (IconType::FileText, "Reading", "Read"),
        "edit" => (IconType::Pencil, "Editing", "Edited"),
        "write" => (IconType::FilePen, "Writing", "Wrote"),
        "grep" | "find" | "glob" | "lsp" => (IconType::Search, "Searching", "Searched"),
        "task" => (IconType::ListTree, "Starting", "Started"),
        "propose_plan" => (IconType::ClipboardList, "Drafting", "Drafted"),
        "wait" => (IconType::Clock, "Waiting", "Waited"),
        "job" => (IconType::Briefcase, "Job", "Job"),
        _ => (IconType::Hammer, tool_name, tool_name),
    };
    (icon, if active { running_verb } else { finished_verb })
}

/// The one-line target a call names: its command, path or pattern.
pub(super) fn argument_summary(tool_name: &str, args_json: &str) -> String {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(args_json) else {
        return String::new();
    };
    if tool_name == "task" {
        let count = parsed
            .get("tasks")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);
        return format!("{count} task{}", if count == 1 { "" } else { "s" });
    }
    if tool_name == "edit" {
        return parsed
            .get("path")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .or_else(|| patch_target(&parsed))
            .unwrap_or_default();
    }
    let value = match tool_name {
        "bash" => parsed.get("command"),
        "read" | "write" | "ls" => parsed.get("path"),
        "grep" | "glob" => parsed.get("pattern"),
        "find" => parsed.get("query").or_else(|| parsed.get("pattern")),
        "lsp" => return joined_fields(&parsed, &["action", "file"]),
        "job" => return joined_fields(&parsed, &["op", "id"]),
        "propose_plan" => parsed.get("title"),
        _ => None,
    };
    value
        .and_then(serde_json::Value::as_str)
        .map(|value| value.lines().next().unwrap_or_default().to_owned())
        .unwrap_or_else(|| compact_arguments(&parsed))
}

fn patch_target(arguments: &serde_json::Value) -> Option<String> {
    arguments
        .as_object()?
        .values()
        .filter_map(serde_json::Value::as_str)
        .find_map(|value| {
            value.lines().find_map(|line| {
                let header = line.strip_prefix('[')?.strip_suffix(']')?;
                let (path, tag) = header.rsplit_once('#')?;
                (!path.is_empty()
                    && tag.len() == 4
                    && tag.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .then(|| path.to_owned())
            })
        })
}
fn compact_arguments(arguments: &serde_json::Value) -> String {
    arguments
        .as_object()
        .map(|object| {
            object
                .iter()
                .filter(|(key, _)| key.as_str() != INTENT_KEY)
                .filter_map(|(_, value)| value.as_str())
                .collect::<Vec<_>>()
                .join(", ")
                .chars()
                .take(100)
                .collect()
        })
        .unwrap_or_default()
}

/// The parts of `keys` the arguments carry as non-empty strings, joined by a space.
fn joined_fields(parsed: &serde_json::Value, keys: &[&str]) -> String {
    keys.iter()
        .filter_map(|key| parsed.get(*key))
        .filter_map(serde_json::Value::as_str)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// One summary clause: the calls that share a verb, and what they count as.
struct SummaryClause {
    verb: String,
    noun: &'static str,
    by_target: bool,
    targets: BTreeSet<String>,
    untargeted: usize,
    calls: usize,
}

impl SummaryClause {
    fn new(verb: String, tool_name: &str) -> Self {
        Self {
            verb,
            noun: noun_for(tool_name),
            by_target: matches!(tool_name, "read" | "edit" | "write"),
            targets: BTreeSet::new(),
            untargeted: 0,
            calls: 0,
        }
    }

    fn record(&mut self, target: String) {
        self.calls += 1;
        if target.is_empty() {
            self.untargeted += 1;
        } else {
            self.targets.insert(target);
        }
    }

    /// File verbs count distinct paths, so re-editing one file reads as one file.
    fn count(&self) -> usize {
        if self.by_target {
            self.targets.len() + self.untargeted
        } else {
            self.calls
        }
    }
}

/// The group header's summary, one clause per verb in first-seen order:
/// "Edited 1 file, read 1 file, ran 1 command".
pub(super) fn group_summary<'a>(calls: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let mut clauses: Vec<SummaryClause> = Vec::new();
    for (tool_name, args_json) in calls {
        let (_, verb) = presentation(tool_name, false);
        let target = argument_summary(tool_name, args_json);
        match clauses.iter_mut().find(|clause| clause.verb == verb) {
            Some(clause) => clause.record(target),
            None => {
                let mut clause = SummaryClause::new(verb.to_owned(), tool_name);
                clause.record(target);
                clauses.push(clause);
            }
        }
    }
    clauses
        .iter()
        .enumerate()
        .map(|(index, clause)| {
            let count = clause.count();
            let verb = if index == 0 {
                clause.verb.clone()
            } else {
                clause.verb.to_lowercase()
            };
            format!("{verb} {count} {}", plural(clause.noun, count))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn noun_for(tool_name: &str) -> &'static str {
    match tool_name {
        "read" | "edit" | "write" => "file",
        "bash" => "command",
        "grep" | "find" | "glob" => "pattern",
        "task" => "agent",
        _ => "call",
    }
}

fn plural(noun: &str, count: usize) -> String {
    if count == 1 {
        noun.to_owned()
    } else {
        format!("{noun}s")
    }
}

/// The call's wall time, when its executor reported one.
pub(super) fn duration_ms(details_json: &str) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(details_json)
        .ok()?
        .get("duration_ms")?
        .as_u64()
}

/// The executor facts worth a line each: exit code, time, truncation, artifact.
pub(super) fn detail_facts(details: Option<&serde_json::Value>) -> Vec<String> {
    let Some(details) = details.and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    let mut facts = Vec::new();
    if let Some(code) = details.get("exit_code").and_then(serde_json::Value::as_i64) {
        facts.push(format!("Exit code {code}"));
    }
    if let Some(ms) = details
        .get("duration_ms")
        .and_then(serde_json::Value::as_u64)
    {
        facts.push(format!("Execution time: {ms} ms"));
    }
    if details
        .get("truncated")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        facts.push("Output was truncated; some output was not transported.".into());
    }
    if let Some(path) = details
        .get("artifact_path")
        .and_then(serde_json::Value::as_str)
    {
        let complete = details
            .get("artifact_complete")
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        facts.push(if complete {
            format!("Complete output artifact: {path}")
        } else {
            format!("Earlier output artifact: {path} (incomplete; full output was not transported)")
        });
    }
    facts
}

/// The model-supplied intent every tool schema carries; it describes the call and is not an argument.
pub(super) const INTENT_KEY: &str = "i";

/// The intent a call declares, normalised the way the working row shows it.
pub(super) fn intent_for(args_json: &str) -> Option<String> {
    let parsed = serde_json::from_str::<serde_json::Value>(args_json).ok()?;
    let raw = parsed.get(INTENT_KEY)?.as_str()?;
    roost_protocol::wire::agent_chat::intent_label(raw)
}

/// The state a call's box is drawn in. A call's own flags win over the
/// tool's error bit, so a cancel or timeout is never drawn as a plain error.
/// `background` is the `async.state` of a backgrounded bash call.
pub(super) fn tool_state(
    queued: bool,
    running: bool,
    cancelled: bool,
    timed_out: bool,
    failed: bool,
    background: Option<&str>,
) -> &'static str {
    if queued {
        "waiting"
    } else if running {
        "running"
    } else if background == Some("running") {
        "background"
    } else if cancelled || background == Some("cancelled") {
        "cancelled"
    } else if timed_out {
        "timeout"
    } else if failed || background == Some("failed") {
        "error"
    } else {
        "done"
    }
}

/// The visible word beside a call's status indicator.
pub(super) fn state_label(state: &str) -> &'static str {
    match state {
        "waiting" => "Waiting",
        "running" => "Running",
        "background" => "Backgrounded",
        "cancelled" => "Cancelled",
        "timeout" => "Timed out",
        "error" => "Error",
        _ => "Done",
    }
}

/// The `StatusDot` status a call's state paints with.
pub(super) fn status_name(state: &str) -> &'static str {
    match state {
        "waiting" => "idle",
        "running" | "background" => "running",
        "cancelled" | "timeout" => "warn",
        "error" => "error",
        _ => "ok",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn edit_argument_summary_extracts_only_a_tagged_patch_filename() {
        let tag = "A1B2";
        let arguments = serde_json::json!({
            "input": format!("[src/main.rs#{tag}]\nPUT 1.=1:\n+replacement")
        });
        assert_eq!(
            argument_summary("edit", &arguments.to_string()),
            "src/main.rs"
        );
        assert_eq!(
            argument_summary("edit", r#"{"patch":"[src/main.rs#wrong]\n-raw patch"}"#),
            ""
        );
    }

    #[test]
    fn a_cancel_or_timeout_is_not_drawn_as_a_plain_error() {
        assert_eq!(
            tool_state(false, false, true, false, true, None),
            "cancelled"
        );
        assert_eq!(tool_state(false, false, false, true, true, None), "timeout");
        assert_eq!(tool_state(false, false, false, false, true, None), "error");
        assert_eq!(
            tool_state(true, false, false, false, false, None),
            "waiting"
        );
    }

    #[test]
    fn a_backgrounded_call_reads_its_job_state() {
        assert_eq!(
            tool_state(false, false, false, false, false, Some("running")),
            "background"
        );
        assert_eq!(
            tool_state(false, false, false, false, false, Some("completed")),
            "done"
        );
        assert_eq!(
            tool_state(false, false, false, false, false, Some("failed")),
            "error"
        );
        assert_eq!(state_label("background"), "Backgrounded");
        assert_eq!(status_name("timeout"), "warn");
    }

    #[test]
    fn group_summary_counts_distinct_files_and_calls() {
        assert_eq!(
            group_summary([
                ("edit", r#"{"path":"a.ts"}"#),
                ("edit", r#"{"path":"a.ts"}"#),
                ("read", r#"{"path":"b.ts"}"#),
                ("bash", r#"{"command":"npm test"}"#),
            ]),
            "Edited 1 file, read 1 file, ran 1 command"
        );
    }
}
