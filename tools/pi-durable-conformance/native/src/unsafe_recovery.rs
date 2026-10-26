//! SIGKILL recovery oracle scenario and its non-idempotent tool.
//! Called by `recovery_traces` with shared SQLite and normalization helpers.
//! Verifies persisted effect intent prevents replay after restart.
use super::*;
pub async fn unsafe_tool_sigkill_recovery() -> Result<Value, Box<dyn Error>> {
    let scratch = ScratchDatabase(scratch_sqlite_path()?);
    let path = scratch.0.as_str();
    let child_path = std::env::current_exe()?
        .parent()
        .ok_or("native executable has no parent directory")?
        .join(child_binary_name());
    if !child_path.is_file() {
        return Err(format!("unsafe child binary is missing: {}", child_path.display()).into());
    }
    let mut child = ChildProcess(
        Command::new(child_path)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let stdout = child
        .stdout
        .take()
        .ok_or("unsafe child stdout is unavailable")?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or("unsafe child stderr is unavailable")?;
    let stderr_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let mut submission_id = None;
    let mut ready = false;
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        if let Some(value) = line.strip_prefix("SUBMISSION:") {
            submission_id = Some(value.parse::<u64>()?);
        }
        if line == "READY" {
            ready = true;
        }
        if submission_id.is_some() && ready {
            break;
        }
    }
    let Some(submission_id) = submission_id else {
        let _ = child.kill();
        let _ = child.wait();
        let stderr = stderr_reader.join().unwrap_or_default();
        return Err(format!("unsafe child did not report a submission; stderr={stderr}").into());
    };
    if !ready {
        let _ = child.kill();
        let _ = child.wait();
        let stderr = stderr_reader.join().unwrap_or_default();
        return Err(format!(
            "unsafe child did not reach its effect-intent boundary; stderr={stderr}"
        )
        .into());
    }
    child.kill()?;
    let status = child.wait()?;
    if !is_sigkill(status) {
        return Err(format!("unsafe child exited without SIGKILL: {}", exit_json(status)).into());
    }
    let _ = stderr_reader.join().unwrap_or_default();

    let counter = Arc::new(AtomicUsize::new(0));
    let (models, provider) = OracleProvider::new([message("recovered", StopReason::Stop, None)]);
    let registry = Arc::new(RegistryImpl::with_builtins(builtin_tasks()));
    registry.install(unsafe_extension(Arc::new(UnsafeCounter(Arc::clone(
        &counter,
    )))))?;
    let storage: Arc<dyn Storage> =
        Arc::new(open_node_sqlite_storage(&path, SqliteDatabaseOptions::default()).await?);
    let harness = open_harness(
        Arc::clone(&storage),
        options(models, registry),
        &BACKGROUND_CONTEXT,
    )
    .await?;
    let submission = harness
        .submission(
            SubmissionId::from_number(submission_id),
            &BACKGROUND_CONTEXT,
        )
        .await?
        .ok_or_else(|| SessionError::message("recovered submission was not found"))?;
    let settled = submission.wait(&BACKGROUND_CONTEXT).await?;
    let root = harness
        .conversation(ConversationId::from_number(1), &BACKGROUND_CONTEXT)
        .await?
        .ok_or_else(|| SessionError::message("recovered root conversation was not found"))?;
    let provider_calls = provider
        .requests
        .lock()
        .map_err(|_| "provider request capture poisoned")?
        .len();
    if provider_calls != 1 {
        return Err(
            format!("expected one provider call after recovery, got {provider_calls}").into(),
        );
    }
    if let Some(error) = provider
        .request_error
        .lock()
        .map_err(|_| "provider error capture poisoned")?
        .as_ref()
    {
        return Err(error.clone().into());
    }
    let trace = json!({
        "name": "unsafe-tool-sigkill-recovery",
        "processExit": exit_json(status),
        "submission": serde_json::to_value(settled)?,
        "entries": serde_json::to_value(entries(root.as_ref()).await?)?,
        "reexecutedUnsafeTool": counter.load(Ordering::SeqCst),
        "providerCallsAfterRestart": provider_calls,
    });
    if counter.load(Ordering::SeqCst) != 0 {
        return Err("unsafe tool was replayed after recovery".into());
    }
    harness.close(&BACKGROUND_CONTEXT).await?;
    storage.close(&BACKGROUND_CONTEXT).await?;
    drop(scratch);
    Ok(trace)
}

fn child_binary_name() -> &'static str {
    if cfg!(windows) {
        "pi-durable-unsafe-child.exe"
    } else {
        "pi-durable-unsafe-child"
    }
}

#[cfg(unix)]
fn exit_json(status: std::process::ExitStatus) -> Value {
    use std::os::unix::process::ExitStatusExt;
    json!({ "code": status.code(), "signal": status.signal().map(signal_name) })
}

#[cfg(unix)]
fn signal_name(signal: i32) -> &'static str {
    if signal == 9 { "SIGKILL" } else { "unknown" }
}

#[cfg(unix)]
fn is_sigkill(status: std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;
    status.signal() == Some(9)
}

#[cfg(not(unix))]
fn is_sigkill(_status: std::process::ExitStatus) -> bool {
    false
}

#[cfg(not(unix))]
fn exit_json(status: std::process::ExitStatus) -> Value {
    json!({ "code": status.code(), "signal": null })
}

struct UnsafeCounter(Arc<AtomicUsize>);

#[async_trait]
impl ToolExecute for UnsafeCounter {
    async fn execute(
        &self,
        _args: JsonObject,
        _api: &dyn ToolExecutionApi,
        _context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(Box::new(SessionError::message(
            "unsafe tool must not be replayed",
        )))
    }
}

fn unsafe_extension(execute: Arc<dyn ToolExecute>) -> Extension {
    Extension {
        name: "unsafe".to_string(),
        tools: vec![ToolRegistration {
            tool: Tool {
                name: "unsafe".to_string(),
                description: "Unsafe work with no automatic replay".to_string(),
                parameters: json!({ "type": "object", "properties": {} }),
                constrained_sampling: None,
            },
            replay: ToolReplay::Unsafe,
            execution_mode: None,
            prepare_arguments: None,
            deferred: false,
            output_limits: None,
            structured_output_schema: None,
            callers: None,
            execute,
        }],
        ..Extension::default()
    }
}

struct ChildProcess(std::process::Child);

impl std::ops::Deref for ChildProcess {
    type Target = std::process::Child;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ChildProcess {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
