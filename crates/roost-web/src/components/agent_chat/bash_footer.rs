//! The dim footer under a bash box: the facts the executor measured, in OMP's
//! order, joined with ` | `. Pure over the call's details and arguments.

use serde_json::Value;

/// Rows of the command a bash box shows. OMP derives this from the terminal
/// height; the web has no such height, so the command keeps a fixed window.
pub(super) const COMMAND_PREVIEW_ROWS: usize = 6;

/// The footer facts for a bash call: backgrounded job, wall time, requested
/// timeout, artifact path, and (for a failure only) the exit code.
pub(super) fn bash_footer_facts(
    details: Option<&Value>,
    args: Option<&Value>,
    is_error: bool,
) -> Vec<String> {
    let mut facts = Vec::new();
    if let Some(job_id) = details
        .and_then(|value| value.get("async"))
        .and_then(|value| value.get("job_id"))
        .and_then(Value::as_str)
    {
        facts.push(format!("Backgrounded: {job_id}"));
    }
    if let Some(duration_ms) = details
        .and_then(|value| value.get("duration_ms"))
        .and_then(Value::as_u64)
    {
        facts.push(format!("Wall: {}", wall_time(duration_ms)));
    }
    if let Some(seconds) = args
        .and_then(|value| value.get("timeout"))
        .and_then(Value::as_u64)
    {
        facts.push(format!("Timeout: {seconds}s"));
    }
    if let Some(path) = details
        .and_then(|value| value.get("artifact_path"))
        .and_then(Value::as_str)
    {
        facts.push(format!("Artifact: {path}"));
    }
    if is_error
        && let Some(code) = details
            .and_then(|value| value.get("exit_code"))
            .and_then(Value::as_i64)
    {
        facts.push(format!("Exit: {code}"));
    }
    facts
}

/// Milliseconds as a wall time: seconds to two decimal places, rounded.
pub(super) fn wall_time(duration_ms: u64) -> String {
    let centiseconds = duration_ms.saturating_add(5) / 10;
    format!("{}.{:02}s", centiseconds / 100, centiseconds % 100)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;

    #[test]
    fn wall_time_has_two_decimal_places() {
        assert_eq!(wall_time(12_300), "12.30s");
        assert_eq!(wall_time(7), "0.01s");
        assert_eq!(wall_time(1_004), "1.00s");
    }

    #[test]
    fn facts_follow_omp_order_and_exit_code_only_on_failure() {
        let details = json!({
            "async": {"state": "running", "job_id": "bg_1"},
            "duration_ms": 12_300,
            "artifact_path": "/tmp/a.log",
            "exit_code": 1
        });
        let args = json!({"command": "cargo test", "timeout": 30});
        assert_eq!(
            bash_footer_facts(Some(&details), Some(&args), true),
            vec![
                "Backgrounded: bg_1",
                "Wall: 12.30s",
                "Timeout: 30s",
                "Artifact: /tmp/a.log",
                "Exit: 1",
            ]
        );
        assert_eq!(
            bash_footer_facts(Some(&details), Some(&args), false).last(),
            Some(&"Artifact: /tmp/a.log".to_owned())
        );
    }

    #[test]
    fn a_call_without_measurements_has_no_footer() {
        assert!(bash_footer_facts(None, Some(&json!({"command": "ls"})), false).is_empty());
    }
}
