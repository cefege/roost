//! The task queue: a coordinator-owned row a worker pulls and claims. No
//! webhook or permission rule takes part in admission; the queue is claimed by
//! whoever asks first and the claim is fenced by its own ttl.
//!
//! `claim_ttl_ms` is the bound every repeated completion check is counted
//! against, so it is required and positive — a zero ttl would make an
//! unfinishable task look expired on the first poll.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::validate::integer_in_range;
use crate::wire::brand::{TaskId, WorkerFp};
use crate::{ProtocolError, ProtocolResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Claimed,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub state: TaskState,
    /// Free-form JSON the worker interprets; the coordinator stores it and
    /// never reads a field out of it.
    pub payload: Map<String, Value>,
    pub enqueued_at_ms: i64,
    pub claimed_at_ms: Option<i64>,
    pub claimed_by: Option<WorkerFp>,
    pub finished_at_ms: Option<i64>,
    pub result: Option<Map<String, Value>>,
    /// A shell-checkable command that tells the worker whether the task's
    /// effect has landed (a merged PR, say). While set, the worker re-checks
    /// until the command exits zero or the claim ttl runs out, which is what
    /// bounds the repeats.
    pub completion_check: Option<String>,
    pub completion_check_last_attempt_ms: Option<i64>,
    pub claim_ttl_ms: i64,
}

impl Task {
    pub fn parse(value: Value) -> ProtocolResult<Self> {
        let task: Task = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("task", error.to_string()))?;
        task.check()?;
        Ok(task)
    }

    pub fn check(&self) -> ProtocolResult<()> {
        integer_in_range("task.enqueued_at_ms", self.enqueued_at_ms, 1, i64::MAX)?;
        integer_in_range("task.claim_ttl_ms", self.claim_ttl_ms, 1, i64::MAX)?;
        for (field, timestamp) in [
            ("task.claimed_at_ms", self.claimed_at_ms),
            ("task.finished_at_ms", self.finished_at_ms),
            (
                "task.completion_check_last_attempt_ms",
                self.completion_check_last_attempt_ms,
            ),
        ] {
            if let Some(timestamp) = timestamp {
                integer_in_range(field, timestamp, 1, i64::MAX)?;
            }
        }
        Ok(())
    }
}

/// Both deltas carry the whole row rather than a field patch, so a consumer
/// applies them the same way whether the task appeared or changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskDelta {
    Created { task: Task },
    State { task: Task },
}

impl TaskDelta {
    pub fn parse(value: Value) -> ProtocolResult<Self> {
        let delta: TaskDelta = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("task_delta", error.to_string()))?;
        match &delta {
            TaskDelta::Created { task } | TaskDelta::State { task } => task.check()?,
        }
        Ok(delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_json() -> Value {
        serde_json::json!({
            "id": "00000000-0000-4000-8000-0000000000b1",
            "state": "pending",
            "payload": { "kind": "deploy" },
            "enqueued_at_ms": 100,
            "claimed_at_ms": null,
            "claimed_by": null,
            "finished_at_ms": null,
            "result": null,
            "completion_check": null,
            "completion_check_last_attempt_ms": null,
            "claim_ttl_ms": 60_000,
        })
    }

    #[test]
    fn a_pending_task_parses_with_a_free_form_payload() {
        let task = Task::parse(task_json()).unwrap();
        assert_eq!(task.state, TaskState::Pending);
        assert_eq!(task.payload["kind"], Value::from("deploy"));
        assert_eq!(task.result, None);
    }

    #[test]
    fn a_zero_claim_ttl_is_rejected() {
        let mut value = task_json();
        value["claim_ttl_ms"] = serde_json::json!(0);
        assert_eq!(Task::parse(value).unwrap_err().field, "task.claim_ttl_ms");
    }

    #[test]
    fn an_unknown_state_is_rejected() {
        let mut value = task_json();
        value["state"] = serde_json::json!("paused");
        assert!(Task::parse(value).is_err());
    }

    #[test]
    fn the_delta_carries_the_row_through_the_same_checks() {
        let mut created = serde_json::json!({ "kind": "created", "task": task_json() });
        assert!(matches!(
            TaskDelta::parse(created.clone()).unwrap(),
            TaskDelta::Created { .. }
        ));
        created["task"]["claim_ttl_ms"] = serde_json::json!(-1);
        assert_eq!(
            TaskDelta::parse(created).unwrap_err().field,
            "task.claim_ttl_ms"
        );
    }
}
