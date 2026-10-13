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
    dispatcher.dispatch(
        CoordWorkerDownstream::AgentToolCall(DAgentToolCall {
            call_id: "write-1".into(),
            conversation_id: "conversation-1".into(),
            cwd: scratch.0.to_string_lossy().into_owned(),
            tool: "write".into(),
            args_json: r#"{"path":"answer.txt","content":"changed"}"#.into(),
            checkpoint_id: 73,
            ..Default::default()
        }),
        Instant::now(),
        &mut TestLink,
    );
    let write_result = timeout(Duration::from_secs(2), async {
        loop {
            if let Some(CoordWorkerUpstream::AgentToolResult(result)) = frames.recv().await
                && result.call_id == "write-1"
            {
                break result;
            }
        }
    })
    .await
    .expect("tool write completes promptly");
    assert!(!write_result.is_error, "{}", write_result.content);
    dispatcher.dispatch(
        CoordWorkerDownstream::AgentToolCall(DAgentToolCall {
            call_id: "rewind-1".into(),
            conversation_id: "conversation-1".into(),
            cwd: scratch.0.to_string_lossy().into_owned(),
            tool: "rewind_files".into(),
            args_json: r#"{"checkpoint_id":73}"#.into(),
            checkpoint_id: 73,
            ..Default::default()
        }),
        Instant::now(),
        &mut TestLink,
    );
    let rewind_result = timeout(Duration::from_secs(2), async {
        loop {
            if let Some(CoordWorkerUpstream::AgentToolResult(result)) = frames.recv().await
                && result.call_id == "rewind-1"
            {
                break result;
            }
        }
    })
    .await
    .expect("file rewind completes promptly");
    assert!(!rewind_result.is_error, "{}", rewind_result.content);
    assert_eq!(
        std::fs::read_to_string(scratch.0.join("answer.txt")).expect("restored answer"),
        "hello\n"
    );
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

#[cfg(unix)]
#[tokio::test]
async fn a_detached_coordinator_link_cancels_running_tool_calls() {
    use roost_worker::link_ports::LinkLifecyclePort;
    use roost_worker::runtime::agent_tools::AgentToolsLinkLifecycle;

    let scratch = Scratch::new();
    let (uplink, mut frames) = uplink::channel();
    let owner = Arc::new(AgentToolsOwner::new(scratch.0.clone(), uplink.clone()));
    let lifecycle = AgentToolsLinkLifecycle(Arc::clone(&owner) as Arc<dyn AgentToolsPort>);
    let dispatcher = dispatcher(uplink, owner);
    let marker = scratch.0.join("finished.txt");
    dispatcher.dispatch(
        CoordWorkerDownstream::AgentToolCall(DAgentToolCall {
            call_id: "bash-detach".into(),
            conversation_id: "conversation-1".into(),
            cwd: scratch.0.to_string_lossy().into_owned(),
            tool: "bash".into(),
            args_json: format!(
                r#"{{"command":"sleep 2 && touch {}"}}"#,
                marker.to_string_lossy()
            ),
            timeout_ms: 60_000,
            ..Default::default()
        }),
        Instant::now(),
        &mut TestLink,
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    lifecycle.on_detach();
    let result = timeout(Duration::from_secs(1), async {
        loop {
            if let Some(CoordWorkerUpstream::AgentToolResult(result)) = frames.recv().await {
                break result;
            }
        }
    })
    .await
    .expect("the detached call settles within one second");
    assert!(result.is_error);
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert!(
        !marker.exists(),
        "the cancelled command did not run to completion"
    );
}
