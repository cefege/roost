//! Portable event-driven interface to the isolated QuickJS WASM worker.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{Map, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    CodemodeCall, CodemodeCallStatus, CodemodeError, CodemodeErrorKind, CodemodeOutputItem,
    CodemodeResult,
    runtime::{
        host::{parse_deep_value, parse_store_writes, serialize_store},
        protocol::{CallTarget, HostToWorker, WorkerToHost},
        worker::{DeterministicStart, WorkerInput, run_worker},
    },
    wasm::WASM_MEMORY_LIMIT_BYTES,
};

#[derive(Debug, Clone)]
pub struct DeclaredTool {
    pub name: String,
    pub js_name: String,
    pub description: String,
}
#[derive(Debug, Clone)]
pub struct DeclaredGlobal {
    pub name: String,
    pub spread: bool,
}
#[derive(Clone)]
pub struct StartOptions {
    pub started_at_ms: u64,
    pub seed: u32,
    pub declared_tools: Vec<DeclaredTool>,
    pub declared_globals: Vec<DeclaredGlobal>,
    pub initial_store: Value,
    pub cancellation_token: CancellationToken,
    pub remaining_timeout: Option<Duration>,
}
impl Default for StartOptions {
    fn default() -> Self {
        Self {
            started_at_ms: 0,
            seed: 0,
            declared_tools: vec![],
            declared_globals: vec![],
            initial_store: Value::Object(Map::new()),
            cancellation_token: CancellationToken::new(),
            remaining_timeout: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Undefined,
    Json(Value),
    Error {
        message: String,
        properties: Option<Map<String, Value>>,
    },
}
pub use crate::runtime::protocol::During;
#[derive(Debug, Clone)]
pub enum Event {
    Started {
        fingerprint: String,
    },
    Call {
        id: u32,
        name: String,
        args: Option<Value>,
        global: Option<String>,
    },
    Idle {
        seq: u64,
        issued: u32,
    },
    Output(CodemodeOutputItem),
    Finished {
        result: CodemodeResult,
        during: Option<During>,
    },
}

/// One isolated WASM worker. Drop and cancellation both terminate the worker.
pub struct CodemodeRun {
    events: mpsc::UnboundedReceiver<WorkerToHost>,
    replies: std::sync::mpsc::Sender<HostToWorker>,
    cancellation: CancellationToken,
    interrupt: Arc<AtomicBool>,
    deadline: Option<tokio::time::Instant>,
    outputs: Vec<CodemodeOutputItem>,
    calls: Vec<CodemodeCall>,
    call_started: std::collections::HashMap<u32, (usize, Instant)>,
    waiting_ids: HashSet<u32>,
    used_seqs: HashSet<u64>,
    terminal: bool,
    lifecycle: CancellationToken,
    worker: Option<thread::JoinHandle<()>>,
}
impl CodemodeRun {
    pub async fn start(code: impl Into<String>, options: StartOptions) -> Result<Self, String> {
        let (events_tx, events) = mpsc::unbounded_channel();
        let (replies, replies_rx) = std::sync::mpsc::channel();
        let interrupt = Arc::new(AtomicBool::new(false));
        let store = match &options.initial_store {
            Value::Object(_) => serialize_store(Some(&options.initial_store)),
            _ => return Err("initial store must be an object".to_owned()),
        };
        let worker_input = WorkerInput {
            code: code.into(),
            tools: options
                .declared_tools
                .into_iter()
                .map(|tool| (tool.name, tool.js_name, tool.description))
                .collect(),
            globals: options
                .declared_globals
                .into_iter()
                .map(|global| (global.name, global.spread))
                .collect(),
            store,
            memory_limit_bytes: Some(WASM_MEMORY_LIMIT_BYTES as u64),
            deterministic: Some(DeterministicStart {
                started_at_ms: options.started_at_ms,
                seed: options.seed,
            }),
            interrupt: interrupt.clone(),
            events: events_tx,
            replies: replies_rx,
        };
        let cancellation = options.cancellation_token;
        let lifecycle = CancellationToken::new();
        let deadline = options
            .remaining_timeout
            .map(|remaining| tokio::time::Instant::now() + remaining);
        if let Some(remaining) = options.remaining_timeout {
            let timer_replies = replies.clone();
            let timer_interrupt = interrupt.clone();
            let timer_lifecycle = lifecycle.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = tokio::time::sleep(remaining) => {
                        timer_interrupt.store(true, Ordering::SeqCst);
                        let _ = timer_replies.send(HostToWorker::Abort);
                    }
                    _ = timer_lifecycle.cancelled() => {}
                }
            });
        }
        let worker = thread::Builder::new()
            .name("rpi-codemode-portable".into())
            .spawn(move || run_worker(worker_input))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            events,
            replies,
            cancellation,
            interrupt,
            deadline,
            outputs: vec![],
            calls: vec![],
            call_started: Default::default(),
            waiting_ids: HashSet::new(),
            used_seqs: HashSet::new(),
            terminal: false,
            lifecycle,
            worker: Some(worker),
        })
    }

    pub async fn next_event(&mut self) -> Result<Event, String> {
        if self.cancellation.is_cancelled() {
            return Ok(self.finish_error(CodemodeErrorKind::Aborted, "codemode run aborted".into()));
        }
        let cancellation = self.cancellation.clone();
        let received = if let Some(deadline) = self.deadline {
            tokio::select! {
                _ = cancellation.cancelled() => Err((CodemodeErrorKind::Aborted, "codemode run aborted")),
                _ = tokio::time::sleep_until(deadline) => Err((CodemodeErrorKind::Timeout, "codemode run timed out")),
                message = self.events.recv() => Ok(message),
            }
        } else {
            tokio::select! {
                _ = cancellation.cancelled() => Err((CodemodeErrorKind::Aborted, "codemode run aborted")),
                message = self.events.recv() => Ok(message),
            }
        };
        let message = match received {
            Ok(message) => message,
            Err((kind, message)) => return Ok(self.finish_error(kind, message.into())),
        };
        if self.cancellation.is_cancelled() {
            return Ok(self.finish_error(CodemodeErrorKind::Aborted, "codemode run aborted".into()));
        }
        if self
            .deadline
            .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
        {
            return Ok(
                self.finish_error(CodemodeErrorKind::Timeout, "codemode run timed out".into())
            );
        }
        match message {
            Some(WorkerToHost::Started { fingerprint }) => Ok(Event::Started { fingerprint }),
            Some(WorkerToHost::Call {
                id,
                target,
                name,
                args,
            }) => {
                let args = match args.as_deref().map(parse_deep_value).transpose() {
                    Ok(args) => args,
                    Err(error) => return self.malformed(error.to_string()),
                };
                if !self.waiting_ids.insert(id) {
                    return self.malformed(format!("duplicate waiting call id {id}"));
                }
                let index = self.calls.len();
                self.calls.push(CodemodeCall {
                    name: name.clone(),
                    status: CodemodeCallStatus::Cancelled,
                    duration_ms: 0.0,
                });
                self.call_started.insert(id, (index, Instant::now()));
                Ok(Event::Call {
                    id,
                    name: name.clone(),
                    args,
                    global: (target == CallTarget::Global).then_some(name),
                })
            }
            Some(WorkerToHost::Output(item)) => {
                self.outputs.push(item.clone());
                Ok(Event::Output(item))
            }
            Some(WorkerToHost::Idle { seq, issued }) => Ok(Event::Idle { seq, issued }),
            Some(WorkerToHost::Done {
                ok,
                value,
                error,
                writes,
                during,
            }) => {
                self.terminal = true;
                self.lifecycle.cancel();
                let calls = std::mem::take(&mut self.calls);
                let result = if ok {
                    let value = match value.as_deref().map(parse_deep_value).transpose() {
                        Ok(value) => value,
                        Err(error) => return self.malformed(error.to_string()),
                    };
                    let (store_writes, warning) = parse_store_writes(writes.as_deref());
                    let mut output = std::mem::take(&mut self.outputs);
                    if let Some(warning) = warning {
                        output.push(CodemodeOutputItem::Text {
                            text: warning,
                            console: false,
                        });
                    }
                    CodemodeResult::Ok {
                        value,
                        output,
                        calls,
                        store_writes,
                    }
                } else {
                    let decoded: Value = match error.as_deref().map(parse_deep_value).transpose() {
                        Ok(Some(error)) => error,
                        Ok(None) => Value::Null,
                        Err(error) => return self.malformed(error.to_string()),
                    };
                    let error = CodemodeError {
                        kind: CodemodeErrorKind::Script,
                        name: decoded
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        message: decoded
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("script failed")
                            .to_owned(),
                        stack: decoded
                            .get("stack")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    };
                    CodemodeResult::Err {
                        error,
                        output: std::mem::take(&mut self.outputs),
                        calls,
                    }
                };
                Ok(Event::Finished {
                    result,
                    during: during.map(|value| During {
                        seq: value.seq,
                        issued: value.issued,
                    }),
                })
            }
            Some(WorkerToHost::Crash { message }) => {
                Ok(self.finish_error(CodemodeErrorKind::Sandbox, message))
            }
            None => Ok(self.finish_error(
                CodemodeErrorKind::Sandbox,
                "codemode worker channel closed before completion".into(),
            )),
        }
    }

    fn finish_error(&mut self, kind: CodemodeErrorKind, message: String) -> Event {
        self.terminal = true;
        self.cancel();
        while let Ok(message) = self.events.try_recv() {
            if let WorkerToHost::Output(item) = message {
                self.outputs.push(item);
            }
        }
        Event::Finished {
            result: CodemodeResult::Err {
                error: CodemodeError {
                    kind,
                    name: None,
                    message,
                    stack: None,
                },
                output: std::mem::take(&mut self.outputs),
                calls: std::mem::take(&mut self.calls),
            },
            during: None,
        }
    }

    fn malformed<T>(&mut self, message: String) -> Result<T, String> {
        self.terminal = true;
        self.cancel();
        Err(message)
    }

    pub fn deliver(
        &mut self,
        seq: u64,
        now_ms: u64,
        id: u32,
        outcome: Outcome,
    ) -> Result<(), String> {
        if self.terminal {
            return Err("codemode run has finished".into());
        }
        if !self.waiting_ids.contains(&id) {
            return Err(format!("call id {id} is not awaiting a result"));
        }
        if seq == 0 || self.used_seqs.contains(&seq) {
            return Err(format!(
                "delivery sequence {seq} is invalid or already used"
            ));
        }
        let (index, started) = *self
            .call_started
            .get(&id)
            .ok_or_else(|| format!("unknown call id {id}"))?;
        let (ok, payload, properties) = match outcome {
            Outcome::Undefined => (true, None, None),
            Outcome::Json(value) => (
                true,
                Some(serde_json::to_string(&value).map_err(|error| error.to_string())?),
                None,
            ),
            Outcome::Error {
                message,
                properties,
            } => (
                false,
                Some(message),
                properties.map(|properties| Value::Object(properties).to_string()),
            ),
        };
        self.waiting_ids.remove(&id);
        self.used_seqs.insert(seq);
        self.call_started.remove(&id);
        self.calls[index].status = if ok {
            CodemodeCallStatus::Ok
        } else {
            CodemodeCallStatus::Error
        };
        self.calls[index].duration_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.replies
            .send(HostToWorker::Result {
                seq,
                now_ms,
                id,
                ok,
                payload,
                properties,
            })
            .map_err(|error| error.to_string())
    }

    pub fn cancel(&mut self) {
        self.interrupt.store(true, Ordering::SeqCst);
        self.cancellation.cancel();
        self.lifecycle.cancel();
        let _ = self.replies.send(HostToWorker::Abort);
    }
}
impl Drop for CodemodeRun {
    fn drop(&mut self) {
        if !self.terminal {
            self.cancel();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
