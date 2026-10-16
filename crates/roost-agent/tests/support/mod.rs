//! Scripted seams for the harness tests: a model whose replies are queued per
//! session id, a worker whose tool answers are fixed per tool name, and a
//! sink that records every event, plus a runtime wired to them.

#![allow(dead_code, unused_imports)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use roost_agent::{
    AgentRuntime, AgentStore, ChatSink, InMemoryAgentStore, Llm, NewConversation, RuntimeConfig,
    ToolCall, ToolExecutor, ToolOutcome,
};
use roost_llm::{AssistantBlock, BlockKind, ChatRequest, StopReason, StreamEvent, Usage};
use roost_protocol::wire::agent_chat::{
    AgentRunState, ChatEvent, ConversationSummary, ModelRef, Transcript, TranscriptItem,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

mod gated;
mod llm;
mod sink;

pub use gated::{ArmOnCheckSink, GatedStore};
pub use llm::{ANY_SESSION, JudgeScript, Script, ScriptedLlm};
pub use sink::RecordingSink;

/// A worker that answers each tool with a fixed outcome and records calls.
#[derive(Default)]
pub struct FakeTools {
    pub answers: Mutex<HashMap<String, ToolOutcome>>,
    pub calls: Mutex<Vec<(String, ToolCall)>>,
    pub delay: Mutex<Option<Duration>>,
    pub panic_next: AtomicBool,
    pub hold_next: AtomicBool,
    pub running_jobs: AtomicUsize,
}

impl FakeTools {
    pub fn answer(&self, tool: &str, content: &str) {
        self.answers.lock().unwrap().insert(
            tool.to_owned(),
            ToolOutcome {
                is_error: false,
                content: content.to_owned(),
                details_json: "{}".into(),
            },
        );
    }

    pub fn calls_to(&self, tool: &str) -> Vec<ToolCall> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, call)| call.tool == tool)
            .map(|(_, call)| call.clone())
            .collect()
    }
}

impl ToolExecutor for FakeTools {
    fn execute<'a>(
        &'a self,
        worker_fp: &'a str,
        call: ToolCall,
        out: mpsc::Sender<String>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ToolOutcome, String>> {
        self.calls
            .lock()
            .unwrap()
            .push((worker_fp.to_owned(), call.clone()));
        let answer = self.answers.lock().unwrap().get(&call.tool).cloned();
        let delay = if call.tool != "context_files" && self.hold_next.swap(false, Ordering::SeqCst)
        {
            Some(Duration::from_secs(3600))
        } else {
            *self.delay.lock().unwrap()
        };
        let panic_next =
            call.tool != "context_files" && self.panic_next.swap(false, Ordering::SeqCst);
        Box::pin(async move {
            if panic_next {
                panic!("simulated worker tool panic");
            }
            if call.tool == "context_files" {
                return Ok(answer.unwrap_or(ToolOutcome {
                    is_error: false,
                    content: r#"{"context":"","watchdog":""}"#.into(),
                    details_json: "{}".into(),
                }));
            }
            let _ = out.send(format!("running {}\n", call.tool)).await;
            if let Some(delay) = delay {
                tokio::select! {
                    () = cancel.cancelled() => return Ok(ToolOutcome { is_error: true, content: "cancelled".into(), details_json: "{}".into() }),
                    () = tokio::time::sleep(delay) => {}
                }
            }
            Ok(answer.unwrap_or(ToolOutcome {
                is_error: false,
                content: format!("{} ok", call.tool),
                details_json: "{}".into(),
            }))
        })
    }

    fn close_conversation<'a>(
        &'a self,
        _worker_fp: &'a str,
        _conversation_id: &'a str,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn detach_background_calls<'a>(&'a self, _: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn abort_background_jobs<'a>(&'a self, _: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn running_background_jobs<'a>(&'a self, _: &'a str) -> BoxFuture<'a, usize> {
        let running = self.running_jobs.load(Ordering::SeqCst);
        Box::pin(async move { running })
    }
}

pub struct Harness {
    pub runtime: AgentRuntime,
    pub llm: Arc<ScriptedLlm>,
    pub tools: Arc<FakeTools>,
    pub sink: Arc<RecordingSink>,
    pub store: Arc<InMemoryAgentStore>,
}

pub const PROVIDER: &str = "anthropic";
pub const MODEL: &str = "claude-sonnet-5";

impl Harness {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let store = Arc::new(InMemoryAgentStore::default());
        let sink = Arc::new(RecordingSink::default());
        Self::from_parts(
            store.clone() as Arc<dyn AgentStore>,
            sink.clone() as Arc<dyn ChatSink>,
            sink,
            store,
        )
    }

    /// A harness whose store parks the loop's record lookup and repeat appends
    /// on request. The sink arms the lookup park when the loop announces its
    /// condition check.
    pub fn gated() -> (Self, Arc<GatedStore>) {
        let inner = Arc::new(InMemoryAgentStore::default());
        let gate = Arc::new(GatedStore::new(inner.clone()));
        let sink = Arc::new(RecordingSink::default());
        let runtime_sink = ArmOnCheckSink {
            inner: sink.clone(),
            gate: gate.clone(),
        };
        let harness = Self::from_parts(
            gate.clone() as Arc<dyn AgentStore>,
            Arc::new(runtime_sink) as Arc<dyn ChatSink>,
            sink,
            inner,
        );
        (harness, gate)
    }

    fn from_parts(
        runtime_store: Arc<dyn AgentStore>,
        runtime_sink: Arc<dyn ChatSink>,
        sink: Arc<RecordingSink>,
        store: Arc<InMemoryAgentStore>,
    ) -> Self {
        let llm = ScriptedLlm::new(&[PROVIDER]);
        let tools = Arc::new(FakeTools::default());
        let config = RuntimeConfig {
            retry_backoff: vec![Duration::from_millis(1); 3],
            advisor_backoff: vec![Duration::from_millis(1); 3],
            find_timeout: Duration::from_secs(5),
            ..RuntimeConfig::default()
        };
        let runtime = AgentRuntime::new(
            runtime_store,
            tools.clone() as Arc<dyn ToolExecutor>,
            runtime_sink,
            llm.clone() as Arc<dyn Llm>,
            config,
        );
        Self {
            runtime,
            llm,
            tools,
            sink,
            store,
        }
    }

    /// A new runtime over the same store, as a restarted coordinator on the
    /// same host: it keeps the instance id, so it adopts its own leases.
    pub fn restart_runtime(&self) -> AgentRuntime {
        AgentRuntime::new(
            self.store.clone() as Arc<dyn AgentStore>,
            self.tools.clone() as Arc<dyn ToolExecutor>,
            self.sink.clone() as Arc<dyn ChatSink>,
            self.llm.clone() as Arc<dyn Llm>,
            RuntimeConfig {
                retry_backoff: vec![Duration::from_millis(1); 3],
                advisor_backoff: vec![Duration::from_millis(1); 3],
                find_timeout: Duration::from_secs(5),
                instance_id: self.runtime.instance_id().to_owned(),
                ..RuntimeConfig::default()
            },
        )
    }

    pub async fn conversation(&self) -> String {
        self.runtime
            .create_conversation(NewConversation {
                worker_fp: "worker-fp".into(),
                worker_label: "desktop".into(),
                worker_os: "linux".into(),
                cwd: "/repo".into(),
                title: None,
                model: Some(ModelRef {
                    provider: PROVIDER.into(),
                    model_id: MODEL.into(),
                }),
                thinking_level: Some("medium".into()),
            })
            .await
            .unwrap()
            .id
    }

    /// Waits until the conversation (and anything it started) settles.
    pub async fn settle(&self, id: &str) -> Transcript {
        self.settle_runtime(&self.runtime, id).await
    }

    pub async fn settle_runtime(&self, runtime: &AgentRuntime, id: &str) -> Transcript {
        for _ in 0..500 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let transcript = runtime.transcript(id).await.unwrap();
            if transcript.run_state != AgentRunState::Running {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let settled = runtime.transcript(id).await.unwrap();
                if settled.run_state != AgentRunState::Running {
                    return settled;
                }
            }
        }
        panic!("conversation {id} did not settle");
    }

    pub async fn submit(&self, id: &str, text: &str) {
        self.runtime.submit(id, text.to_owned()).await.unwrap();
    }
}

pub fn usage() -> StreamEvent {
    StreamEvent::Usage(Usage {
        input: 100,
        output: 20,
        cache_read: 0,
        cache_write: 0,
    })
}

pub fn text_reply(text: &str) -> Script {
    Ok(vec![
        StreamEvent::BlockStart {
            index: 0,
            kind: BlockKind::Text,
        },
        StreamEvent::TextDelta {
            index: 0,
            text: text.to_owned(),
        },
        StreamEvent::BlockEnd {
            index: 0,
            block: AssistantBlock::Text {
                text: text.to_owned(),
            },
        },
        usage(),
        StreamEvent::Stop(StopReason::EndTurn),
    ])
}

pub fn tool_reply(calls: &[(&str, &str, &str)]) -> Script {
    let mut events = Vec::new();
    for (index, (call_id, name, args)) in calls.iter().enumerate() {
        events.push(StreamEvent::ToolCallStart {
            index,
            call_id: (*call_id).to_owned(),
            name: (*name).to_owned(),
        });
        events.push(StreamEvent::ToolArgsDelta {
            index,
            json: (*args).to_owned(),
        });
        events.push(StreamEvent::BlockEnd {
            index,
            block: AssistantBlock::ToolCall {
                call_id: (*call_id).to_owned(),
                name: (*name).to_owned(),
                args_json: (*args).to_owned(),
            },
        });
    }
    events.push(usage());
    events.push(StreamEvent::Stop(StopReason::ToolUse));
    Ok(events)
}

pub fn texts(transcript: &Transcript) -> Vec<String> {
    transcript
        .items
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::Assistant { blocks, .. } => Some(
                blocks
                    .iter()
                    .filter_map(|block| match block {
                        roost_protocol::wire::agent_chat::TranscriptBlock::Text { text } => {
                            Some(text.clone())
                        }
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

/// The message text of every user-role message in a request.
pub fn user_texts(request: &ChatRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .filter_map(|message| match message {
            roost_llm::Message::User { content } => Some(
                content
                    .iter()
                    .filter_map(|part| match part {
                        roost_llm::UserContent::Text { text } => Some(text.clone()),
                        roost_llm::UserContent::Image { .. } => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}
