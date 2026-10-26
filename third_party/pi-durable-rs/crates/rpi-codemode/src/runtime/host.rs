//! Async host side of the sandbox (port of `CodemodeSandbox` / `Execution`
//! in `packages/codemode/src/runtime/host.ts` @ abe508e1b).
//!
//! One script run gets its own worker thread, fresh wasmtime `Store`, and
//! fresh QuickJS VM. A runaway script, including one that only spins the
//! microtask queue, is interrupted and cannot poison a later run.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::identifier::to_codemode_identifier;
use crate::runtime::protocol::{CallTarget, HostToWorker, WorkerToHost};
use crate::runtime::worker::{WorkerInput, run_worker};
use crate::types::{
    CodemodeCall, CodemodeCallStatus, CodemodeError, CodemodeErrorKind, CodemodeExecuteOptions,
    CodemodeOutputItem, CodemodeResult, CodemodeSandboxOptions, CodemodeStoreWrites,
    CodemodeTimeout, CodemodeTool, CodemodeToolContext, RESERVED_GLOBALS,
};

/// Number of live `run_worker` threads. Test-only: the V16-07 O-C regression
/// test asserts it returns to its previous value after the host abandons an
/// execution whose tool never resolves.
#[cfg(test)]
static LIVE_WORKERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Decrements [`LIVE_WORKERS`] when a worker thread exits (test builds).
#[cfg(test)]
struct WorkerThreadGuard;

#[cfg(test)]
impl Drop for WorkerThreadGuard {
    fn drop(&mut self) {
        LIVE_WORKERS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One in-flight call as the host tracks it (`PendingCall`, host.ts:102-106).
struct PendingCall {
    /// Tool calls get a record; globals are tracked for the reply only.
    index: Option<usize>,
    started_at: Instant,
}

/// Call records plus the pending index (`Execution.output/calls/pending`).
#[derive(Default)]
struct CallTable {
    records: Vec<CodemodeCall>,
    by_id: HashMap<u32, PendingCall>,
}

/// `CodemodeSandbox`: holds the tool table and defaults; every `execute()`
/// gets its own worker and VM.
pub struct CodemodeSandbox {
    /// Insertion-ordered so `tools`/`ALL_TOOLS` expose registration order
    /// (upstream `Map` semantics).
    tools: Mutex<Vec<CodemodeTool>>,
    globals: Mutex<Vec<CodemodeTool>>,
    timeout: CodemodeTimeout,
    memory_limit_bytes: Option<u64>,
    closed: AtomicBool,
    running: Mutex<HashMap<u64, CancellationToken>>,
    next_execution_id: Mutex<u64>,
    idle: tokio::sync::Notify,
    /// Test-only hook: report a sandbox error before starting a worker
    /// (mirrors upstream's injectable `wasm`/`workerUrl` failure paths).
    #[doc(hidden)]
    pub fail_engine: Option<String>,
}

impl CodemodeSandbox {
    /// Build a sandbox; `Err` mirrors the upstream constructor throws for
    /// invalid/reserved/conflicting global names and duplicate tools.
    pub fn new(options: CodemodeSandboxOptions) -> Result<Self, String> {
        let mut tools: Vec<CodemodeTool> = Vec::new();
        for tool in options.tools {
            if tools.iter().any(|existing| existing.name == tool.name) {
                return Err(format!("Tool \"{}\" is already registered", tool.name));
            }
            tools.push(tool);
        }
        let mut globals: Vec<CodemodeTool> = Vec::new();
        let mut namespaces: Vec<String> = Vec::new();
        for global in options.globals {
            let parts: Vec<&str> = global.name.split('.').collect();
            if parts.len() > 2
                || parts.iter().any(|part| !is_identifier(part))
                || RESERVED_GLOBALS.contains(&parts[0])
            {
                return Err(format!("Invalid global name \"{}\"", global.name));
            }
            if globals.iter().any(|existing| existing.name == global.name) {
                return Err(format!("Global \"{}\" is already registered", global.name));
            }
            if parts.len() == 2 && !namespaces.contains(&parts[0].to_owned()) {
                namespaces.push(parts[0].to_owned());
            }
            globals.push(global);
        }
        for namespace in namespaces {
            if globals.iter().any(|global| global.name == namespace) {
                return Err(format!(
                    "Global \"{namespace}\" conflicts with the namespace \"{namespace}\""
                ));
            }
        }
        Ok(Self {
            tools: Mutex::new(tools),
            globals: Mutex::new(globals),
            timeout: options.timeout_ms,
            memory_limit_bytes: options.memory_limit_bytes,
            closed: AtomicBool::new(false),
            running: Mutex::new(HashMap::new()),
            next_execution_id: Mutex::new(1),
            idle: tokio::sync::Notify::new(),
            fail_engine: None,
        })
    }

    /// Throws if a tool with the same name is already registered.
    pub fn register_tool(&self, tool: CodemodeTool) -> Result<(), String> {
        let mut tools = lock(&self.tools);
        if tools.iter().any(|existing| existing.name == tool.name) {
            return Err(format!("Tool \"{}\" is already registered", tool.name));
        }
        tools.push(tool);
        Ok(())
    }

    pub fn unregister_tool(&self, name: &str) -> bool {
        let mut tools = lock(&self.tools);
        let before = tools.len();
        tools.retain(|tool| tool.name != name);
        tools.len() != before
    }

    pub fn tools(&self) -> Vec<CodemodeTool> {
        lock(&self.tools).clone()
    }

    pub fn globals(&self) -> Vec<CodemodeTool> {
        lock(&self.globals).clone()
    }

    /// `execute(code, options)`: `code` is an async function body, so
    /// `return` and top-level `await` work. Never `Err` for script failures;
    /// `Err` only when the sandbox is closed (upstream: a rejected promise).
    pub async fn execute(
        &self,
        code: &str,
        options: CodemodeExecuteOptions,
    ) -> Result<CodemodeResult, String> {
        if self.closed.load(Ordering::SeqCst) {
            return Err("Sandbox is closed".to_owned());
        }
        let execution_id = {
            let mut next = lock(&self.next_execution_id);
            let id = *next;
            *next += 1;
            id
        };
        let abort = CancellationToken::new();
        lock(&self.running).insert(execution_id, abort.clone());
        let result = self.run_execution(code, options, abort).await;
        lock(&self.running).remove(&execution_id);
        // `notify_waiters` (not `notify_one`): every concurrent `close()`
        // caller must observe the last execution settling, otherwise one of
        // them hangs forever (v0.1.6 review P3).
        self.idle.notify_waiters();
        result
    }

    /// Abort in-flight executions (they resolve with `kind: "aborted"`) and
    /// reject new ones; waits until every in-flight execution has settled.
    pub async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let tokens: Vec<CancellationToken> = lock(&self.running).values().cloned().collect();
        for token in tokens {
            token.cancel();
        }
        loop {
            // Register the waiter before re-checking the running set
            // (v0.1.6 review round 2, O1): `Notified` joins the wait queue
            // only on its first poll, so the old check→await order could
            // miss the last `notify_waiters` (which stores no permit) in
            // the window between the check and the await and wait forever.
            let notified = self.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if lock(&self.running).is_empty() {
                break;
            }
            notified.await;
        }
    }

    async fn run_execution(
        &self,
        code: &str,
        options: CodemodeExecuteOptions,
        abort: CancellationToken,
    ) -> Result<CodemodeResult, String> {
        if let Some(message) = &self.fail_engine {
            return Ok(CodemodeResult::Err {
                error: CodemodeError {
                    kind: CodemodeErrorKind::Sandbox,
                    name: None,
                    message: message.clone(),
                    stack: None,
                },
                output: Vec::new(),
                calls: Vec::new(),
            });
        }
        let tools: Vec<CodemodeTool> = lock(&self.tools).clone();
        let globals: Vec<CodemodeTool> = lock(&self.globals).clone();
        let effective_timeout = options.timeout_ms.unwrap_or(self.timeout);
        let deadline = effective_timeout
            .effective_ms()
            .map(|ms| Instant::now() + Duration::from_millis(ms));

        let interrupt = Arc::new(AtomicBool::new(false));
        // The caller's cancellation signal joins the sandbox-internal abort
        // token (v0.1.6 review P0-2): upstream attaches an `abort` listener
        // to the signal that finishes the execution, so Esc must settle the
        // host future even when the script never settles on its own.
        let external_signal = options.signal.clone();
        let abort_for_cancel = abort.clone();
        let cancelled = async move {
            match external_signal {
                Some(signal) => {
                    tokio::select! {
                        _ = abort_for_cancel.cancelled() => {}
                        _ = signal.cancelled() => {}
                    }
                }
                None => abort_for_cancel.cancelled().await,
            }
        };
        tokio::pin!(cancelled);
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (replies_tx, replies_rx) = std::sync::mpsc::channel();
        let worker_input = WorkerInput {
            code: code.to_owned(),
            tools: tools
                .iter()
                .map(|tool| {
                    (
                        tool.name.clone(),
                        to_codemode_identifier(&tool.name),
                        tool.description.clone().unwrap_or_default(),
                    )
                })
                .collect(),
            globals: globals
                .iter()
                .map(|global| (global.name.clone(), global.spread))
                .collect(),
            store: serialize_store(options.store.as_ref()),
            memory_limit_bytes: self.memory_limit_bytes,
            deterministic: None,
            interrupt: interrupt.clone(),
            events: events_tx,
            replies: replies_rx,
        };
        #[cfg(test)]
        LIVE_WORKERS.fetch_add(1, Ordering::SeqCst);
        let handle = std::thread::spawn(move || {
            #[cfg(test)]
            let _guard = WorkerThreadGuard;
            run_worker(worker_input)
        });

        let calls = Arc::new(Mutex::new(CallTable::default()));
        let mut output: Vec<CodemodeOutputItem> = Vec::new();
        // Output is bounded inside the VM instead: the prelude fails the
        // script past `MAX_OUTPUT_CHARS` / `MAX_OUTPUT_ITEMS` (#10283), which
        // is also what upstream does.
        let abort_message = |closed: bool| {
            if closed {
                "Sandbox closed"
            } else {
                "Execution aborted"
            }
        };

        let outcome: CodemodeResult;
        // Set when the host gives up on the execution (abort or timeout).
        let mut abandoned = false;
        loop {
            let timeout_sleep = async {
                match deadline {
                    Some(deadline) => {
                        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                biased;
                _ = &mut cancelled => {
                    interrupt.store(true, Ordering::SeqCst);
                    abandoned = true;
                    outcome = CodemodeResult::Err {
                        error: CodemodeError {
                            kind: CodemodeErrorKind::Aborted,
                            name: None,
                            message: abort_message(self.closed.load(Ordering::SeqCst)).to_owned(),
                            stack: None,
                        },
                        output: std::mem::take(&mut output),
                        calls: finish_calls(&calls),
                    };
                    break;
                }
                _ = timeout_sleep => {
                    interrupt.store(true, Ordering::SeqCst);
                    abandoned = true;
                    outcome = CodemodeResult::Err {
                        error: CodemodeError {
                            kind: CodemodeErrorKind::Timeout,
                            name: None,
                            message: match effective_timeout.effective_ms() {
                                Some(ms) => format!("Execution timed out after {ms} ms"),
                                None => "Execution timed out".to_owned(),
                            },
                            stack: None,
                        },
                        output: std::mem::take(&mut output),
                        calls: finish_calls(&calls),
                    };
                    break;
                }
                message = events_rx.recv() => {
                    match message {
                        Some(WorkerToHost::Output(item)) => {
                            output.push(item);
                        }
                        Some(WorkerToHost::Call { id, target, name, args }) => {
                            let source = if target == CallTarget::Tool {
                                lookup_tool(&tools, &name)
                            } else {
                                lookup_tool(&globals, &name)
                            };
                            let is_tool = target == CallTarget::Tool;
                            let child = abort.child_token();
                            {
                                let mut table = lock(&calls);
                                let index = if is_tool {
                                    let index = table.records.len();
                                    table.records.push(CodemodeCall {
                                        name: name.clone(),
                                        status: CodemodeCallStatus::Cancelled,
                                        duration_ms: 0.0,
                                    });
                                    Some(index)
                                } else {
                                    None
                                };
                                table.by_id.insert(id, PendingCall {
                                    index,
                                    started_at: Instant::now(),
                                });
                            }
                            let Some(tool) = source else {
                                fail_call(&calls, &replies_tx, id, &format!(
                                    "Unknown {} \"{name}\"",
                                    if is_tool { "tool" } else { "global" }
                                ));
                                continue;
                            };
                            let parsed = match args {
                                None => Ok(Value::Null),
                                Some(text) => parse_deep_value(&text).map_err(|error| error.to_string()),
                            };
                            let args_value = match parsed {
                                Ok(value) => value,
                                Err(error) => {
                                    fail_call(&calls, &replies_tx, id, &error);
                                    continue;
                                }
                            };
                            let calls_for_task = calls.clone();
                            let replies = replies_tx.clone();
                            let execution = abort.clone();
                            tokio::spawn(async move {
                                let result = (tool.execute)(
                                    args_value,
                                    CodemodeToolContext { signal: child.clone() },
                                )
                                .await;
                                complete_call(&calls_for_task, &replies, id, result, &execution);
                            });
                        }
                        Some(WorkerToHost::Done { ok, value, writes, error, during: _ }) => {
                            let (store_writes, store_warning) =
                                parse_store_writes(writes.as_deref());
                            if let Some(warning) = store_warning {
                                output.push(CodemodeOutputItem::Text { text: warning, console: false });
                            }
                            // A return value that cannot be decoded (over the
                            // nesting bound) is reported, not silently NULLed
                            // (v0.1.6 review follow-up).
                            let parsed_value = if ok {
                                match value {
                                    Some(text) => match parse_deep_value(&text) {
                                        Ok(value) => Some(value),
                                        Err(_) => {
                                            output.push(CodemodeOutputItem::Text {
                                                text: format!(
                                                    "Warning: the script return value was discarded (JSON nesting limit {MAX_JSON_DEPTH})."
                                                ),
                                                console: false,
                                            });
                                            None
                                        }
                                    },
                                    None => None,
                                }
                            } else {
                                None
                            };
                            outcome = if ok {
                                CodemodeResult::Ok {
                                    value: parsed_value,
                                    output: std::mem::take(&mut output),
                                    calls: finish_calls(&calls),
                                    store_writes,
                                }
                            } else {
                                CodemodeResult::Err {
                                    error: parse_script_error(error.as_deref()),
                                    output: std::mem::take(&mut output),
                                    calls: finish_calls(&calls),
                                }
                            };
                            break;
                        }
                        Some(WorkerToHost::Started { .. } | WorkerToHost::Idle { .. }) => {}
                        Some(WorkerToHost::Crash { message }) => {
                            outcome = CodemodeResult::Err {
                                error: CodemodeError {
                                    kind: CodemodeErrorKind::Sandbox,
                                    name: None,
                                    message,
                                    stack: None,
                                },
                                output: std::mem::take(&mut output),
                                calls: finish_calls(&calls),
                            };
                            break;
                        }
                        None => {
                            outcome = CodemodeResult::Err {
                                error: CodemodeError {
                                    kind: CodemodeErrorKind::Sandbox,
                                    name: None,
                                    message: "Worker exited before the script settled".to_owned(),
                                    stack: None,
                                },
                                output: std::mem::take(&mut output),
                                calls: finish_calls(&calls),
                            };
                            break;
                        }
                    }
                }
            }
        }

        // A host that abandons an execution must wake a worker blocked on
        // `replies.recv()`: dropping `replies_tx` is not enough while an
        // in-flight tool task still holds a sender clone, and the interrupt
        // flag is only observed while wasm runs (V16-07 review O-C).
        if abandoned {
            let _ = replies_tx.send(HostToWorker::Abort);
        }
        // The interrupt flag is set for every outcome, like upstream
        // `finish()` (`Atomics.store`, host.ts:332): a script that reported
        // `done` and kept looping (for example `try { text(x) } catch {}`
        // after the output limit failed it) must not spin its worker thread.
        interrupt.store(true, Ordering::SeqCst);
        // Already cancelled by finish(): the record keeps "cancelled" and the
        // worker is gone or going (host.ts:297-308).
        abort.cancel();
        drop(replies_tx);
        drop(handle);
        Ok(outcome)
    }
}

/// Tool lookup by raw name.
fn lookup_tool(tools: &[CodemodeTool], name: &str) -> Option<CodemodeTool> {
    tools.iter().find(|tool| tool.name == name).cloned()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' || first == '$' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
}

pub(crate) fn serialize_store(store: Option<&Value>) -> serde_json::Map<String, Value> {
    let mut serialized = serde_json::Map::new();
    if let Some(Value::Object(entries)) = store {
        for (key, value) in entries {
            serialized.insert(key.clone(), Value::String(value.to_string()));
        }
    }
    serialized
}

/// `parseStoreWrites` (host.ts:64-81): entries of `[key, json]` (set) and
/// `[key]` (delete). Returns a warning when a value could not be decoded
/// (deep nesting), so the loss is visible instead of silent (v0.1.6 review
/// P3).
pub(crate) fn parse_store_writes(json: Option<&str>) -> (CodemodeStoreWrites, Option<String>) {
    let mut writes = CodemodeStoreWrites::default();
    let mut failed: Vec<String> = Vec::new();
    let Some(json) = json else {
        return (writes, None);
    };
    let Ok(Value::Array(entries)) = parse_deep_value(json) else {
        return (writes, None);
    };
    for entry in entries {
        let Some(entry) = entry.as_array() else {
            continue;
        };
        let Some(key) = entry.first().and_then(Value::as_str) else {
            continue;
        };
        if entry.len() < 2 {
            writes.delete.push(key.to_owned());
        } else if let Some(Value::String(value)) = entry.get(1) {
            // The worker sends `[key, json]` where `json` is the value's JSON
            // text; `parseStoreWrites` parses it back (host.ts:64-81).
            match parse_deep_value(value) {
                Ok(parsed) => {
                    writes.set.insert(key.to_owned(), parsed);
                }
                Err(_) => failed.push(key.to_owned()),
            }
        }
    }
    let warning = (!failed.is_empty()).then(|| {
        format!(
            "Warning: codemode store values for {:?} were not persisted (JSON nesting limit {MAX_JSON_DEPTH}).",
            failed
        )
    });
    (writes, warning)
}

/// Maximum JSON nesting accepted from the sandbox. serde_json's default
/// limit (128) silently dropped deep codemode store and return values;
/// upstream `JSON.parse` handles roughly 10^4 levels but fails beyond that,
/// so this keeps a bounded, explicit limit (v0.1.6 review P3).
const MAX_JSON_DEPTH: usize = 1024;

/// Nesting depth of a JSON document (string/escape aware, iterative).
fn json_depth(text: &str) -> usize {
    let mut depth = 0usize;
    let mut max = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for byte in text.bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                max = max.max(depth);
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

/// Parse a JSON value with [`MAX_JSON_DEPTH`] as the recursion bound
/// (serde_json's recursion limit is disabled only after this guard).
pub(crate) fn parse_deep_value(text: &str) -> Result<Value, serde_json::Error> {
    if json_depth(text) > MAX_JSON_DEPTH {
        return Err(<serde_json::Error as serde::de::Error>::custom(format!(
            "JSON nesting exceeds {MAX_JSON_DEPTH} levels"
        )));
    }
    let mut deserializer = serde_json::Deserializer::from_str(text);
    deserializer.disable_recursion_limit();
    let value = serde::Deserialize::deserialize(&mut deserializer)?;
    // The recursion limit was disabled for this deserializer; `end()` still
    // enforces a single complete document so trailing garbage is rejected
    // (round-2).
    deserializer.end()?;
    Ok(value)
}

/// `handleDone` error branch (host.ts:255-263): `{name?, message, stack?}`.
fn parse_script_error(error: Option<&str>) -> CodemodeError {
    let parsed = error
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|value| value.as_object().cloned());
    match parsed {
        Some(fields) => CodemodeError {
            kind: CodemodeErrorKind::Script,
            name: fields
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned),
            message: fields
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            stack: fields
                .get("stack")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        None => CodemodeError {
            kind: CodemodeErrorKind::Script,
            name: None,
            message: error.unwrap_or("Unknown script error").to_owned(),
            stack: None,
        },
    }
}

/// Update a call that failed before/without running the tool.
fn fail_call(
    calls: &Arc<Mutex<CallTable>>,
    replies: &std::sync::mpsc::Sender<HostToWorker>,
    id: u32,
    error: &str,
) {
    {
        let mut table = lock(calls);
        if let Some(pending) = table.by_id.remove(&id)
            && let Some(index) = pending.index
        {
            table.records[index].status = CodemodeCallStatus::Error;
            table.records[index].duration_ms = pending.started_at.elapsed().as_secs_f64() * 1000.0;
        }
    }
    let _ = replies.send(HostToWorker::Result {
        seq: 1,
        now_ms: 0,
        id,
        ok: false,
        payload: Some(error.to_owned()),
        properties: None,
    });
}

/// Record a completed call and reply to the worker; a call that finished
/// after the execution did is ignored for the record but still replies (the
/// worker may be gone, in which case the send is dropped).
fn complete_call(
    calls: &Arc<Mutex<CallTable>>,
    replies: &std::sync::mpsc::Sender<HostToWorker>,
    id: u32,
    result: Result<Value, String>,
    execution: &CancellationToken,
) {
    let payload;
    let ok;
    {
        let mut table = lock(calls);
        match table.by_id.remove(&id) {
            Some(pending) => {
                let duration = pending.started_at.elapsed().as_secs_f64() * 1000.0;
                if let Some(index) = pending.index {
                    table.records[index].duration_ms = duration;
                    // A call cut off by finish() keeps `cancelled`, even when
                    // the tool surfaces its own cancellation as an error
                    // (host.ts:297-308).
                    match result {
                        Ok(value) if !execution.is_cancelled() => {
                            table.records[index].status = CodemodeCallStatus::Ok;
                            ok = true;
                            payload = Some(value.to_string());
                        }
                        Ok(value) => {
                            table.records[index].status = CodemodeCallStatus::Cancelled;
                            ok = true;
                            payload = Some(value.to_string());
                        }
                        Err(error) if execution.is_cancelled() => {
                            table.records[index].status = CodemodeCallStatus::Cancelled;
                            ok = false;
                            payload = Some(error);
                        }
                        Err(error) => {
                            table.records[index].status = CodemodeCallStatus::Error;
                            ok = false;
                            payload = Some(error);
                        }
                    }
                } else {
                    match result {
                        Ok(value) => {
                            ok = true;
                            payload = Some(value.to_string());
                        }
                        Err(error) => {
                            ok = false;
                            payload = Some(error);
                        }
                    }
                }
            }
            // After finish(), no reply is needed (host.ts:297-308).
            None => return,
        }
    }
    let _ = replies.send(HostToWorker::Result {
        seq: 1,
        now_ms: 0,
        id,
        ok,
        payload,
        properties: None,
    });
}

/// `finish()` call bookkeeping: pending calls get their duration and stay
/// `cancelled`; running records are snapshotted.
fn finish_calls(calls: &Arc<Mutex<CallTable>>) -> Vec<CodemodeCall> {
    let mut table = lock(calls);
    let now = Instant::now();
    let pending: Vec<(Option<usize>, Instant)> = table
        .by_id
        .values()
        .map(|call| (call.index, call.started_at))
        .collect();
    for (index, started_at) in pending {
        if let Some(index) = index {
            table.records[index].duration_ms =
                now.duration_since(started_at).as_secs_f64() * 1000.0;
        }
    }
    table.by_id.clear();
    table.records.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tool that reports its invocation and then never settles, ignoring
    /// the cancellation signal.
    fn hanging_tool(called: tokio::sync::mpsc::UnboundedSender<()>) -> CodemodeTool {
        CodemodeTool {
            name: "hang".to_owned(),
            description: None,
            input_schema: None,
            output_schema: None,
            spread: false,
            signature: None,
            execute: Arc::new(move |_args, _ctx| {
                let called = called.clone();
                Box::pin(async move {
                    let _ = called.send(());
                    std::future::pending::<Result<Value, String>>().await
                })
            }),
        }
    }

    /// V16-07 review O-C: a worker blocked on `replies.recv()` while an
    /// in-flight tool never resolves must still exit when the host abandons
    /// the execution. Without the `HostToWorker::Abort` message the thread
    /// (and its VM) outlived the sandbox.
    #[tokio::test]
    async fn abandoning_an_execution_releases_a_worker_blocked_on_a_hanging_tool() {
        let (called_tx, mut called_rx) = tokio::sync::mpsc::unbounded_channel();
        let sandbox = Arc::new(
            CodemodeSandbox::new(CodemodeSandboxOptions {
                tools: vec![hanging_tool(called_tx)],
                timeout_ms: CodemodeTimeout::Milliseconds(60_000),
                ..Default::default()
            })
            .expect("sandbox"),
        );
        let before = LIVE_WORKERS.load(Ordering::SeqCst);
        let running = {
            let sandbox = sandbox.clone();
            tokio::spawn(async move {
                sandbox
                    .execute(
                        "await tools.hang(); return 'never'",
                        CodemodeExecuteOptions::default(),
                    )
                    .await
                    .expect("execution")
            })
        };
        called_rx.recv().await.expect("tool invoked");
        sandbox.close().await;
        let result = running.await.expect("join");
        let CodemodeResult::Err { error, .. } = result else {
            panic!("expected aborted, got {result:?}");
        };
        assert_eq!(error.kind, CodemodeErrorKind::Aborted);

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while LIVE_WORKERS.load(Ordering::SeqCst) > before {
            assert!(
                tokio::time::Instant::now() < deadline,
                "worker thread leaked after the host abandoned the execution"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// v0.1.6 review P3: two concurrent `close()` callers must both return
    /// once the in-flight execution settles (`notify_one` left one of them
    /// waiting forever).
    ///
    /// Round-2 review O1 fixed the companion missed-wakeup window
    /// (`Notified` only registers on its first poll, so close() now pins
    /// and enables the waiter before re-checking `running`). That window is
    /// an instruction-level multi-thread preemption between the check and
    /// the await; it cannot be opened deterministically from a test, so
    /// this test does not claim to cover it.
    #[tokio::test]
    async fn concurrent_close_callers_both_return() {
        let (called_tx, mut called_rx) = tokio::sync::mpsc::unbounded_channel();
        let sandbox = Arc::new(
            CodemodeSandbox::new(CodemodeSandboxOptions {
                tools: vec![hanging_tool(called_tx)],
                timeout_ms: CodemodeTimeout::Milliseconds(60_000),
                ..Default::default()
            })
            .expect("sandbox"),
        );
        let running = {
            let sandbox = sandbox.clone();
            tokio::spawn(async move {
                sandbox
                    .execute(
                        "await tools.hang(); return 'never'",
                        CodemodeExecuteOptions::default(),
                    )
                    .await
                    .expect("execution")
            })
        };
        called_rx.recv().await.expect("tool invoked");
        let first = sandbox.clone();
        let second = sandbox.clone();
        let closes = async move {
            let (a, b) = tokio::join!(first.close(), second.close());
            (a, b)
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            let _ = &running;
            closes.await;
        })
        .await
        .expect("both close callers must return");
        let _ = running.await.expect("join");
    }

    /// Round-2: the depth-disabled deserializer still requires one complete
    /// JSON document; trailing garbage must be rejected.
    #[test]
    fn parse_deep_value_rejects_trailing_garbage() {
        assert!(parse_deep_value("{\"ok\":true} trailing").is_err());
        assert!(parse_deep_value("1 2").is_err());
        assert!(parse_deep_value("[1,2] extra").is_err());
        assert!(parse_deep_value("{\"ok\":true}").is_ok());
        assert!(parse_deep_value("[1,2]").is_ok());
    }

    /// v0.1.6 review P3: deep store values survive the default 128-level
    /// serde limit; beyond the bounded limit the loss is reported.
    #[test]
    fn deep_store_values_round_trip_and_overflows_are_reported() {
        let deep = format!("{}1{}", "[".repeat(512), "]".repeat(512));
        let writes_json = serde_json::json!([["k", deep]]).to_string();
        let (writes, warning) = parse_store_writes(Some(&writes_json));
        assert!(warning.is_none(), "{warning:?}");
        assert!(writes.set.contains_key("k"));
        let too_deep = format!(
            "{}1{}",
            "[".repeat(MAX_JSON_DEPTH + 10),
            "]".repeat(MAX_JSON_DEPTH + 10)
        );
        let writes_json = serde_json::json!([["k", too_deep]]).to_string();
        let (writes, warning) = parse_store_writes(Some(&writes_json));
        assert!(writes.set.is_empty());
        assert!(
            warning.unwrap().contains("not persisted"),
            "the dropped value must be reported"
        );
        // The scanner ignores brackets inside strings.
        assert_eq!(json_depth(r#"{"a":"[[[[","b":[[1]]}"#), 3);
    }

    /// v0.1.6 review follow-up: a return value beyond the nesting bound is
    /// discarded with a visible warning, not silently NULLed.
    #[tokio::test]
    async fn over_deep_return_values_are_reported() {
        let sandbox = CodemodeSandbox::new(CodemodeSandboxOptions {
            timeout_ms: CodemodeTimeout::Milliseconds(60_000),
            ..Default::default()
        })
        .expect("sandbox");
        let result = sandbox
            .execute(
                "let v = 1; for (let i = 0; i < 1200; i++) { v = [v]; } return v;",
                CodemodeExecuteOptions::default(),
            )
            .await
            .expect("execution");
        let CodemodeResult::Ok { value, output, .. } = result else {
            panic!("expected ok, got {result:?}");
        };
        assert!(value.is_none(), "over-deep return value must be discarded");
        assert!(
            output.iter().any(|item| matches!(item, CodemodeOutputItem::Text { text, .. } if text.contains("return value was discarded"))),
            "expected the return-value warning in {output:?}"
        );
    }

    /// v0.1.6 review P0-2: the caller's cancellation signal must settle the
    /// host execution even when the script never settles on its own
    /// (upstream attaches an `abort` listener to the signal).
    #[tokio::test]
    async fn external_signal_aborts_a_spinning_script() {
        let sandbox = Arc::new(
            CodemodeSandbox::new(CodemodeSandboxOptions {
                timeout_ms: CodemodeTimeout::Infinite,
                ..Default::default()
            })
            .expect("sandbox"),
        );
        let before = LIVE_WORKERS.load(Ordering::SeqCst);
        let signal = CancellationToken::new();
        let running = {
            let sandbox = sandbox.clone();
            let signal = signal.clone();
            tokio::spawn(async move {
                sandbox
                    .execute(
                        "while (true) {}",
                        CodemodeExecuteOptions {
                            signal: Some(signal),
                            ..Default::default()
                        },
                    )
                    .await
                    .expect("execution")
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        signal.cancel();
        let result = tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .expect("abort must settle the execution")
            .expect("join");
        let CodemodeResult::Err { error, .. } = result else {
            panic!("expected aborted, got {result:?}");
        };
        assert_eq!(error.kind, CodemodeErrorKind::Aborted);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while LIVE_WORKERS.load(Ordering::SeqCst) > before {
            assert!(
                tokio::time::Instant::now() < deadline,
                "worker thread leaked after the signal abort"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// v0.1.6 review P1-4: a script that catches the QuickJS interrupt and
    /// keeps calling `console.log` (which replenishes fuel) must not spin
    /// the abandoned worker thread forever — the interrupt now leaves one
    /// unit of fuel, so the next wasm instruction traps uncatchably.
    #[tokio::test]
    async fn timeout_kills_a_script_that_catches_the_interrupt() {
        let sandbox = Arc::new(
            CodemodeSandbox::new(CodemodeSandboxOptions {
                timeout_ms: CodemodeTimeout::Milliseconds(100),
                ..Default::default()
            })
            .expect("sandbox"),
        );
        let before = LIVE_WORKERS.load(Ordering::SeqCst);
        let result = sandbox
            .execute(
                "while (true) { try { console.log('x'); } catch (error) {} }",
                CodemodeExecuteOptions::default(),
            )
            .await
            .expect("execution");
        let CodemodeResult::Err { error, .. } = result else {
            panic!("expected timeout, got {result:?}");
        };
        assert_eq!(error.kind, CodemodeErrorKind::Timeout);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while LIVE_WORKERS.load(Ordering::SeqCst) > before {
            assert!(
                tokio::time::Instant::now() < deadline,
                "worker thread spun after the timeout"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// #10283: a print loop past the prelude's output limits fails the
    /// script instead of accumulating without bound (the limits themselves
    /// are asserted in `sandbox_behavior_test.rs`).
    #[tokio::test]
    async fn output_past_the_prelude_limit_fails_the_script() {
        let sandbox = CodemodeSandbox::new(CodemodeSandboxOptions {
            timeout_ms: CodemodeTimeout::Milliseconds(60_000),
            ..Default::default()
        })
        .expect("sandbox");
        let result = sandbox
            .execute(
                "for (;;) { text('x'.repeat(1 << 20)); }",
                CodemodeExecuteOptions::default(),
            )
            .await
            .expect("execution");
        let CodemodeResult::Err { error, output, .. } = result else {
            panic!("expected the output limit to fail the script");
        };
        assert_eq!(error.kind, CodemodeErrorKind::Script);
        assert_eq!(error.name.as_deref(), Some("RangeError"));
        assert!(
            error.message.contains("script output exceeded"),
            "{}",
            error.message
        );
        let chars: usize = output
            .iter()
            .map(|item| match item {
                CodemodeOutputItem::Text { text, .. } => text.len(),
                CodemodeOutputItem::Image { data, .. } => data.len(),
            })
            .sum();
        assert!(
            chars <= crate::types::MAX_OUTPUT_CHARS,
            "output limit not enforced: {chars} chars"
        );
    }
}
