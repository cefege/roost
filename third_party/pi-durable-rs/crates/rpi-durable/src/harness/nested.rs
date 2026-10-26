//! Durable nested-call receipts owned by the calling tool task.
//!
//! `execute_tool` uses this family to reserve stable keyed calls and to publish
//! each child's terminal result without adding nested calls to the transcript.

use std::sync::{Arc, LazyLock};

use serde::{Deserialize, Serialize};

use crate::chord::json::{JsonObject, JsonValue};
use crate::documents::{DocDefinition, DocScope, DocToken, define_doc_family};
use crate::harness::types::NestedToolExecutionResult;
use crate::ids::TaskId;

/// Durable identity and receipt of one call made by a tool task.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NestedCallState {
    /// Child task; absent only if admission failed before task creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    /// Call signature used to reject reuse of a key for different work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<JsonObject>,
    /// Exactly the terminal value returned by `execute_tool`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<NestedToolExecutionResult>,
}

fn initial() -> JsonObject {
    match serde_json::to_value(NestedCallState::default()) {
        Ok(JsonValue::Object(object)) => object,
        _ => JsonObject::new(),
    }
}

/// Task-scoped family of nested call states, retired with the parent task.
pub(crate) static NESTED_CALL_DOC: LazyLock<DocToken> = LazyLock::new(|| {
    define_doc_family(DocDefinition {
        kind: "pi.tool.nested-call".to_string(),
        version: 1,
        scope: DocScope::Task,
        history: None,
        fork: None,
        family: true,
        initial: Arc::new(|_| initial()),
        migrate: None,
        checkpoint_when: Some(Arc::new(|_, _, _| false)),
    })
});
