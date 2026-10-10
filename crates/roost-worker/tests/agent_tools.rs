//! Coordinator tool frames reach the worker's native tool host and cancel commands promptly.
//!
//! A real downstream dispatcher routes production protobuf payloads to a tool owner.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "link_downstream_support/mod.rs"]
mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use roost_proto::DAgentToolCall;
#[cfg(unix)]
use roost_proto::DAgentToolCancel;
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};
use roost_worker::browser_commands::Command;
use roost_worker::link_ports::{AgentToolsPort, DownstreamOwners, LinkPipelineState};
use roost_worker::runtime::agent_tools::AgentToolsOwner;
use roost_worker::runtime::downstream::{Dispatcher, DownstreamLink};
use roost_worker::uplink::{self, LinkFence, Uplink};
use tokio::time::timeout;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("roost-agent-tools-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path).expect("scratch directory is created");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct TestLink;

impl DownstreamLink for TestLink {
    fn reply(&mut self, _frame: CoordWorkerUpstream) {}
    fn hello_acknowledged(&mut self, _terminal_metadata_negotiated: bool) {}
    fn event_acknowledged(&mut self, _client_seq: u64) -> bool {
        true
    }
    fn browser_command(&mut self, _command: Command, _fence: LinkFence) {}
    fn pipeline_state(&self) -> LinkPipelineState {
        LinkPipelineState::default()
    }
}

fn dispatcher(uplink: Uplink, owner: Arc<AgentToolsOwner>) -> Dispatcher {
    let mut owners: DownstreamOwners = support::Fakes::new(support::OwnerMode::Answer).owners();
    owners.agent_tools = Some(owner as Arc<dyn AgentToolsPort>);
    Dispatcher::new(uplink, "epoch", Some(owners))
}

#[tokio::test]
async fn agent_tool_call_reads_file_and_returns_hashline_result() {
    let scratch = Scratch::new();
    std::fs::write(scratch.0.join("answer.txt"), "hello\n").expect("fixture file is written");
    let (uplink, mut frames) = uplink::channel();
    let owner = Arc::new(AgentToolsOwner::new(scratch.0.clone(), uplink.clone()));
    let dispatcher = dispatcher(uplink, owner);
    dispatcher.dispatch(
        CoordWorkerDownstream::AgentToolCall(DAgentToolCall {
            call_id: "read-1".into(),
            conversation_id: "conversation-1".into(),
            cwd: scratch.0.to_string_lossy().into_owned(),
            tool: "read".into(),
            args_json: r#"{"path":"answer.txt"}"#.into(),
            timeout_ms: 5_000,
            ..Default::default()
        }),
        Instant::now(),
        &mut TestLink,
    );
    let result = timeout(Duration::from_secs(2), async {
        loop {
            if let Some(CoordWorkerUpstream::AgentToolResult(result)) = frames.recv().await {
                break result;
            }
        }
    })
    .await
    .expect("tool read completes promptly");
    assert!(!result.is_error, "{}", result.content);
    assert!(
        result.content.contains("[answer.txt#5BF9]"),
        "{}",
        result.content
    );
    assert!(result.content.contains("1:hello"), "{}", result.content);
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_agent_bash_returns_error_within_one_second() {
    let scratch = Scratch::new();
    let (uplink, mut frames) = uplink::channel();
    let owner = Arc::new(AgentToolsOwner::new(scratch.0.clone(), uplink.clone()));
    let dispatcher = dispatcher(uplink, owner);
    dispatcher.dispatch(
        CoordWorkerDownstream::AgentToolCall(DAgentToolCall {
            call_id: "bash-1".into(),
            conversation_id: "conversation-1".into(),
            cwd: scratch.0.to_string_lossy().into_owned(),
            tool: "bash".into(),
            args_json: r#"{"command":"sleep 30"}"#.into(),
            timeout_ms: 60_000,
            ..Default::default()
        }),
        Instant::now(),
        &mut TestLink,
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    dispatcher.dispatch(
        CoordWorkerDownstream::AgentToolCancel(DAgentToolCancel {
            call_id: "bash-1".into(),
            ..Default::default()
        }),
        Instant::now(),
        &mut TestLink,
    );
    let result = timeout(Duration::from_secs(1), async {
        loop {
            if let Some(CoordWorkerUpstream::AgentToolResult(result)) = frames.recv().await {
                break result;
            }
        }
    })
    .await
    .expect("cancelled bash settles within one second");
    assert!(result.is_error);
}
