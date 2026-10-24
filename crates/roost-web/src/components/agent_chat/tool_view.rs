//! What one tool call shows, as pure functions: its verb and target, the
//! slice of output visible while folded (OMP's per-tool preview windows), and
//! the executor facts. The component is `tool_card`.
use libero::components::SvgData;

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
pub(super) fn presentation(tool_name: &str, active: bool) -> (SvgData, String) {
    let (icon, running_verb, finished_verb) = match tool_name {
        "bash" => (pictogram_icons_lucide::terminal::outlined, "Running", "Ran"),
        "read" => (
            pictogram_icons_lucide::file_text::outlined,
            "Reading",
            "Read",
        ),
        "edit" => (
            pictogram_icons_lucide::pencil::outlined,
            "Editing",
            "Edited",
        ),
        "write" => (
            pictogram_icons_lucide::file_pen::outlined,
            "Writing",
            "Wrote",
        ),
        "grep" | "find" | "glob" | "lsp" => (
            pictogram_icons_lucide::search::outlined,
            "Searching",
            "Searched",
        ),
        "task" => (
            pictogram_icons_lucide::list_tree::outlined,
            "Starting",
            "Started",
        ),
        "propose_plan" => (
            pictogram_icons_lucide::clipboard_list::outlined,
            "Drafting",
            "Drafted",
        ),
        "wait" => (pictogram_icons_lucide::clock::outlined, "Waiting", "Waited"),
        "job" => (pictogram_icons_lucide::briefcase::outlined, "Job", "Job"),
        _ => (
            pictogram_icons_lucide::hammer::outlined,
            tool_name,
            tool_name,
        ),
    };
    (
        icon,
        if active { running_verb } else { finished_verb }.into(),
    )
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
    let value = match tool_name {
        "bash" => parsed.get("command"),
        "read" | "write" | "edit" | "ls" => parsed.get("path"),
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

/// The `Indicator` color a call's state maps to.
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
    fn the_intent_is_read_from_args_and_never_summarised() {
        assert_eq!(
            intent_for(r#"{"i":"Run the suite","command":"cargo test"}"#).as_deref(),
            Some("Run the suite")
        );
        assert_eq!(intent_for(r#"{"command":"cargo test"}"#), None);
        assert_eq!(
            argument_summary("bash", r#"{"i":"Run","command":"cargo test"}"#),
            "cargo test"
        );
        assert_eq!(argument_summary("wait", r#"{"i":"Wait for jobs"}"#), "");
        assert_eq!(
            argument_summary("job", r#"{"i":"Check","op":"output","id":"bg_1"}"#),
            "output bg_1"
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
}
