//! What one tool call shows, as pure functions: its verb and target, the
//! slice of output visible while folded (OMP's per-tool preview windows), and
//! the executor facts. The component is `tool_card`.

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

/// The icon and verb for a call in its current state.
pub(super) fn presentation(
    tool_name: &str,
    queued: bool,
    running: bool,
    cancelled: bool,
    timed_out: bool,
) -> (&'static str, String) {
    let (icon, active, finished) = match tool_name {
        "bash" => ("terminal", "Running", "Ran"),
        "read" => ("description", "Reading", "Read"),
        "edit" => ("edit", "Editing", "Edited"),
        "write" => ("edit_document", "Writing", "Wrote"),
        "grep" | "find" | "glob" | "lsp" => ("search", "Searching", "Searched"),
        "task" => ("account_tree", "Starting", "Started"),
        "propose_plan" => ("assignment", "Drafting", "Drafted"),
        _ => ("build", tool_name, tool_name),
    };
    (
        icon,
        if cancelled {
            "Cancelled".into()
        } else if timed_out {
            "Timed out".into()
        } else if queued {
            "Waiting".into()
        } else if running {
            active.into()
        } else {
            finished.into()
        },
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
        "lsp" => {
            return [parsed.get("action"), parsed.get("file")]
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
        }
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
                .values()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
                .chars()
                .take(100)
                .collect()
        })
        .unwrap_or_default()
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
