//! Fork-policy, sleep/wake, ownership-abort, and busy-queue oracle traces.
//! Scenario implementations are grouped into focused sibling modules.
//! Producers use the shipped durable Harness and Session APIs, not fixture data.

use crate::provider::{OracleProvider, message};
use crate::scenarios::options;
use async_trait::async_trait;
use rpi_ai::types::StopReason;
use rpi_durable::chord::context::{BACKGROUND_CONTEXT, Context};
use rpi_durable::chord::json::{JsonObject, JsonValue};
use rpi_durable::documents::{DocDefinition, DocScope, DocToken, define_doc};
use rpi_durable::harness::{
    AnyTask, HarnessOptions, NextTaskState, RegistryImpl, RunningTask, TaskDefinition, TaskRuntime,
    builtin_tasks, open_harness,
};
use rpi_durable::session::{DocOwner, EntryDraft, TaskCreate, TaskOwnership, commit_fn};
use rpi_durable::storage::MemoryStorage;
use rpi_durable::types::{DocumentFork, DocumentHistory, TaskOutcome, TaskState};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::error::Error;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, Semaphore};

type ScenarioResult<T> = Result<T, Box<dyn Error>>;

fn context() -> &'static Context {
    &BACKGROUND_CONTEXT
}

fn doc(kind: &str, history: DocumentHistory, fork: DocumentFork) -> DocToken {
    define_doc(DocDefinition {
        kind: kind.to_string(),
        version: 1,
        scope: DocScope::Conversation,
        history: Some(history),
        fork: Some(fork),
        family: false,
        initial: Arc::new(|_| {
            let mut object = JsonObject::new();
            object.insert("value".into(), JsonValue::String("initial".into()));
            object
        }),
        migrate: None,
        checkpoint_when: Some(Arc::new(|_, _, _| false)),
    })
}

fn set_value(draft: &rpi_durable::session::DocDraft, value: &str) -> Result<(), String> {
    let mut root = draft.write().map_err(|error| error.to_string())?;
    let object = root
        .as_object_mut()
        .ok_or_else(|| "document root was not an object".to_string())?;
    object.insert("value".into(), JsonValue::String(value.into()));
    Ok(())
}
#[path = "fork_tasks.rs"]
mod fork_tasks;
#[path = "queue_admission.rs"]
mod queue_admission;

pub use fork_tasks::{durable_sleep_wake, fork_document_policies, generic_task_ownership_abort};
pub use queue_admission::busy_steer_followup_reject_withdraw;
