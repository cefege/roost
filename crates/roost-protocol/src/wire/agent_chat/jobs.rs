//! Background bash job contract shared by the worker, coordinator and clients.
//! Owns the job lifecycle states, the `job` tool arguments, and the
//! `details_json.async` block that marks a bash result as backgrounded, so no
//! crate parses or formats that shape by hand.

use serde::{Deserialize, Serialize};
use serde_json::json;

/// Lifecycle of one background job. A job starts `Running` and settles exactly once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub fn is_settled(self) -> bool {
        self != Self::Running
    }

    /// A settled job delivers its result as a message that starts a run on an
    /// idle conversation. A cancelled job is only journaled for the next run.
    pub fn wakes_conversation(self) -> bool {
        !matches!(self, Self::Cancelled)
    }
}

/// The `async` block of a bash result's `details_json`: the job it started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsyncDetails {
    pub state: JobState,
    pub job_id: String,
}

#[derive(Deserialize)]
struct AsyncEnvelope {
    #[serde(rename = "async")]
    async_block: AsyncDetails,
}

/// `details_json` for a bash result that runs (or ran) as a background job.
pub fn async_details_json(state: JobState, job_id: &str) -> String {
    json!({"async": {"state": state.as_str(), "job_id": job_id}}).to_string()
}

/// The job a bash result started, or `None` when the result was not backgrounded.
pub fn parse_async_details(details_json: &str) -> Option<AsyncDetails> {
    serde_json::from_str::<AsyncEnvelope>(details_json)
        .ok()
        .map(|envelope| envelope.async_block)
}

/// Operations of the model-visible `job` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobOp {
    List,
    Output,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobArgs {
    pub op: JobOp,
    #[serde(default)]
    pub id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn async_details_round_trip_and_reject_plain_bash_details() {
        let json = async_details_json(JobState::Running, "bg_3");
        assert_eq!(
            parse_async_details(&json),
            Some(AsyncDetails {
                state: JobState::Running,
                job_id: "bg_3".into(),
            })
        );
        assert_eq!(parse_async_details(r#"{"exit_code":0}"#), None);
    }

    #[test]
    fn job_state_parse_matches_as_str() {
        for state in [
            JobState::Running,
            JobState::Completed,
            JobState::Failed,
            JobState::Cancelled,
        ] {
            assert_eq!(JobState::parse(state.as_str()), Some(state));
        }
        assert_eq!(JobState::parse("lost"), None);
    }
}
