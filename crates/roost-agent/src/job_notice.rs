//! Model-facing text for a settled background job. History delivers it as a
//! user-role context message, so the model reads a job result the way it reads
//! other system notices. Kept out of `history.rs`, which is at its size cap.

use roost_protocol::wire::agent_chat::JobState;

/// The `<system-notice>` message that reports one job to the model.
pub fn background_job_message(
    job_id: &str,
    command: &str,
    state: JobState,
    output: &str,
    exit_code: Option<i32>,
) -> String {
    let headline = match state {
        JobState::Running => format!("Background job {job_id} is still running."),
        JobState::Completed => {
            format!(
                "Background job {job_id} has completed. Resume your work using the result below."
            )
        }
        JobState::Failed => {
            format!("Background job {job_id} has failed. Resume your work using the result below.")
        }
        JobState::Cancelled => format!("Background job {job_id} was cancelled."),
    };
    let exit = exit_code.map_or_else(String::new, |code| format!("\nExit code: {code}"));
    format!("<system-notice>\n{headline}\n\nCommand: {command}{exit}\n\n{output}\n</system-notice>")
}

/// The compaction-summary line for a job: its command and a bounded output head.
pub fn summary_text(job_id: &str, command: &str, output: &str) -> String {
    let shown: String = output.chars().take(4096).collect();
    format!("\n[background job {job_id}] {command}\n{shown}\n")
}
