#![allow(dead_code)]

//! Shared support for the durable tool tests (V17-15): a minimal
//! [`ToolExecutionApi`] with an environment, collected output and diagnostics,
//! mirroring the upstream `fakeApi` test double, plus temporary directories.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rpi_durable::chord::context::Context;
use rpi_durable::chord::json::{JsonObject, JsonValue};
use rpi_durable::env::types::ExecutionEnv;
use rpi_durable::harness::types::{
    Agent, ConversationHandle, RegistrySnapshot, SettledTask, ToolCommitFn, ToolDiagnostic,
    ToolExecuteError, ToolExecutionApi, ToolExecutionResult, ToolRegistration,
};
use rpi_durable::ids::{ConversationId, TaskId, id_from_number};
use rpi_durable::session::SessionError;
use rpi_durable::types::TaskRecord;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

/// A temporary directory removed when the test process exits.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Create a fresh directory under the system temp root.
    pub fn new(prefix: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    /// The directory path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Path as a string.
    pub fn path_str(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The upstream `fakeApi`: an environment, collected output, diagnostics and
/// details, and nothing durable.
#[derive(Clone)]
pub struct FakeApi {
    /// Execution environment offered to the tool.
    pub env: Option<Arc<dyn ExecutionEnv>>,
    /// Text streamed through `output()`.
    pub output: Arc<Mutex<String>>,
    /// Diagnostics recorded through `diagnostic()`.
    pub diagnostics: Arc<Mutex<Vec<ToolDiagnostic>>>,
    /// Last value recorded through `details()`.
    pub details: Arc<Mutex<Option<JsonValue>>>,
}

impl FakeApi {
    /// Build the fake for `env`.
    pub fn new(env: Option<Arc<dyn ExecutionEnv>>) -> Self {
        Self {
            env,
            output: Arc::new(Mutex::new(String::new())),
            diagnostics: Arc::new(Mutex::new(Vec::new())),
            details: Arc::new(Mutex::new(None)),
        }
    }

    /// Collected output text.
    pub fn output_text(&self) -> String {
        self.output.lock().expect("output").clone()
    }

    /// Collected diagnostics.
    pub fn diagnostic_list(&self) -> Vec<ToolDiagnostic> {
        self.diagnostics.lock().expect("diagnostics").clone()
    }
}

#[async_trait]
impl rpi_durable::harness::types::DocumentReader for FakeApi {
    async fn snapshot(
        &self,
        _token: &rpi_durable::DocToken,
        _owner: rpi_durable::session::DocOwner<'_>,
        _context: &Context,
    ) -> Result<Option<JsonObject>, SessionError> {
        Ok(None)
    }
}

#[async_trait]
impl ToolExecutionApi for FakeApi {
    fn shared(&self) -> Arc<dyn ToolExecutionApi> {
        Arc::new(self.clone())
    }
    async fn execute_tool(
        &self,
        _name: &str,
        _arguments: rpi_durable::chord::json::JsonObject,
        _options: rpi_durable::harness::types::NestedToolOptions,
        _context: &Context,
    ) -> Result<rpi_durable::harness::types::NestedToolExecutionResult, SessionError> {
        Err(SessionError::message(
            "nested calls are not used by tool unit tests",
        ))
    }
    fn now(&self) -> Result<f64, SessionError> {
        Ok(0.0)
    }
    fn task_id(&self) -> TaskId {
        id_from_number(1)
    }

    fn conversation_id(&self) -> ConversationId {
        id_from_number(1)
    }

    fn call_id(&self) -> &str {
        "call"
    }

    fn registry(&self) -> Arc<dyn RegistrySnapshot> {
        unimplemented!("registry is not used by the tool tests")
    }

    async fn agent(&self, _context: &Context) -> Result<Agent, SessionError> {
        unimplemented!("agent is not used by the tool tests")
    }

    fn models(&self) -> &rpi_ai::models::Models {
        unimplemented!("models are not used by the tool tests")
    }

    fn env(&self) -> Option<Arc<dyn ExecutionEnv>> {
        self.env.clone()
    }

    fn output(
        &self,
        chunk: &[u8],
        _skipped: Option<rpi_durable::env::types::ShellOutputSkip>,
    ) -> Result<(), String> {
        self.output
            .lock()
            .expect("output")
            .push_str(&String::from_utf8_lossy(chunk));
        Ok(())
    }

    fn output_window(&self) -> Option<rpi_durable::env::types::ShellOutputWindow> {
        None
    }

    fn diagnostic(&self, diagnostic: ToolDiagnostic) {
        self.diagnostics
            .lock()
            .expect("diagnostics")
            .push(diagnostic);
    }

    async fn details(&self, value: JsonValue, _context: &Context) -> Result<(), SessionError> {
        *self.details.lock().expect("details") = Some(value);
        Ok(())
    }

    async fn commit(
        &self,
        _change: ToolCommitFn,
        _context: &Context,
    ) -> Result<JsonValue, SessionError> {
        unimplemented!("commit is not used by the tool tests")
    }

    async fn memo(
        &self,
        _name: &str,
        _context: &Context,
    ) -> Result<Option<JsonValue>, SessionError> {
        unimplemented!("memo is not used by the tool tests")
    }

    async fn memo_set(
        &self,
        _name: &str,
        _value: JsonValue,
        _context: &Context,
    ) -> Result<JsonValue, SessionError> {
        unimplemented!("memo_set is not used by the tool tests")
    }

    async fn get_task(
        &self,
        _id: TaskId,
        _context: &Context,
    ) -> Result<Option<TaskRecord>, SessionError> {
        unimplemented!("get_task is not used by the tool tests")
    }

    async fn wait_for_task(
        &self,
        _id: TaskId,
        _context: &Context,
    ) -> Result<SettledTask<JsonValue>, SessionError> {
        unimplemented!("wait_for_task is not used by the tool tests")
    }

    async fn conversation(
        &self,
        _id: ConversationId,
        _context: &Context,
    ) -> Result<Option<Arc<dyn ConversationHandle>>, SessionError> {
        unimplemented!("conversation is not used by the tool tests")
    }
}

/// Run `tool` against `env`, returning the result, streamed output and
/// diagnostics.
pub async fn run_tool(
    tool: &ToolRegistration,
    args: JsonValue,
    env: Option<Arc<dyn ExecutionEnv>>,
) -> (
    Result<ToolExecutionResult, ToolExecuteError>,
    String,
    Vec<ToolDiagnostic>,
) {
    let api = FakeApi::new(env);
    let context = rpi_durable::chord::context::BACKGROUND_CONTEXT.clone();
    let args: JsonObject = args.as_object().cloned().unwrap_or_default();
    let result = tool.execute.execute(args, &api, &context).await;
    let output = api.output_text();
    let diagnostics = api.diagnostic_list();
    (result, output, diagnostics)
}

/// Text of a tool result's content.
pub fn text_output(result: &ToolExecutionResult) -> String {
    result
        .content
        .as_ref()
        .map(|content| {
            content
                .iter()
                .filter_map(|part| match part {
                    rpi_ai::types::ToolResultContent::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Diagnostic message text.
pub fn diagnostic_text(result: &ToolExecutionResult) -> String {
    result
        .diagnostics
        .as_ref()
        .map(|diagnostics| {
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}
