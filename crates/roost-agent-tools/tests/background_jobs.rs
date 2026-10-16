//! Behaviour tests for background `bash` jobs: async return, auto-background,
//! grandchild stdout holding a job open, and `job` cancel.
//! Drives `ToolHost::call` with real shell processes and reads settlements from the channel.

use std::path::PathBuf;
use std::time::Duration;

use roost_agent_tools::{SettledJob, ToolCallRequest, ToolHost};
use roost_protocol::wire::agent_chat::{JobArgs, JobOp, TOOL_BASH, TOOL_JOB};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const SETTLE_WAIT: Duration = Duration::from_secs(10);

fn host_with_settlements() -> (ToolHost, mpsc::UnboundedReceiver<SettledJob>, PathBuf) {
    let data_dir = tempfile::tempdir()
        .expect("temporary data directory")
        .keep();
    let (settled_sender, settled_receiver) = mpsc::unbounded_channel();
    let host = ToolHost::new(data_dir.clone(), settled_sender);
    (host, settled_receiver, data_dir)
}

fn request(call_id: &str, tool: &str, args_json: String) -> ToolCallRequest {
    ToolCallRequest {
        call_id: call_id.into(),
        conversation_id: "conversation-1".into(),
        cwd: std::env::temp_dir(),
        tool: tool.into(),
        args_json,
        timeout_ms: 60_000,
        checkpoint_id: 0,
    }
}

async fn next_settlement(receiver: &mut mpsc::UnboundedReceiver<SettledJob>) -> SettledJob {
    tokio::time::timeout(SETTLE_WAIT, receiver.recv())
        .await
        .expect("a job settles within the wait")
        .expect("settlement channel stays open")
}

fn bash_args(command: &str, background: bool) -> String {
    serde_json::json!({ "command": command, "async": background }).to_string()
}

#[tokio::test]
async fn async_bash_returns_job_id_at_once_and_settles_later() {
    let (host, mut settlements, _dir) = host_with_settlements();
    let started = std::time::Instant::now();
    let outcome = host
        .call(
            request(
                "call-1",
                TOOL_BASH,
                bash_args("sleep 1; echo finished", true),
            ),
            mpsc::channel(8).0,
            CancellationToken::new(),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.content);
    assert!(started.elapsed() < Duration::from_millis(900));
    let settled = next_settlement(&mut settlements).await;
    assert_eq!(settled.conversation_id, "conversation-1");
    assert_eq!(settled.exit_code, Some(0));
    assert!(settled.output.contains("finished"));
    assert!(!settled.is_error);
}

#[tokio::test]
async fn foreground_bash_auto_backgrounds_after_threshold() {
    let (host, mut settlements, _dir) = host_with_settlements();
    let host = host.with_auto_background_after(Duration::from_millis(100));
    let outcome = host
        .call(
            request(
                "call-2",
                TOOL_BASH,
                bash_args("sleep 0.5; echo late", false),
            ),
            mpsc::channel(8).0,
            CancellationToken::new(),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.content);
    let settled = next_settlement(&mut settlements).await;
    assert!(settled.output.contains("late"));
}

#[tokio::test]
async fn grandchild_holding_stdout_does_not_keep_job_pending() {
    let (host, mut settlements, _dir) = host_with_settlements();
    host.call(
        request(
            "call-3",
            TOOL_BASH,
            bash_args("(sleep 30 &); echo shell-done", true),
        ),
        mpsc::channel(8).0,
        CancellationToken::new(),
    )
    .await;
    let settled = next_settlement(&mut settlements).await;
    assert!(settled.output.contains("shell-done"));
    assert_eq!(settled.exit_code, Some(0));
}

#[tokio::test]
async fn job_cancel_settles_running_job_as_cancelled() {
    let (host, mut settlements, _dir) = host_with_settlements();
    host.call(
        request("call-4", TOOL_BASH, bash_args("sleep 30", true)),
        mpsc::channel(8).0,
        CancellationToken::new(),
    )
    .await;
    let list = host
        .call(
            request(
                "call-5",
                TOOL_JOB,
                serde_json::to_string(&JobArgs {
                    op: JobOp::List,
                    id: None,
                })
                .expect("job args serialise"),
            ),
            mpsc::channel(8).0,
            CancellationToken::new(),
        )
        .await;
    let job_id = list
        .content
        .split_whitespace()
        .find(|word| word.starts_with("bg_"))
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_owned()
        })
        .expect("list names the running job");
    let cancel = host
        .call(
            request(
                "call-6",
                TOOL_JOB,
                serde_json::to_string(&JobArgs {
                    op: JobOp::Cancel,
                    id: Some(job_id),
                })
                .expect("job args serialise"),
            ),
            mpsc::channel(8).0,
            CancellationToken::new(),
        )
        .await;
    assert!(!cancel.is_error, "{}", cancel.content);
    let settled = next_settlement(&mut settlements).await;
    assert_eq!(settled.state.as_str(), "cancelled");
}

#[tokio::test]
async fn sixteenth_running_job_is_refused_with_the_limit_message() {
    let (host, _settlements, _dir) = host_with_settlements();
    for call_index in 0..15 {
        let started = host
            .call(
                request(
                    &format!("call-{call_index}"),
                    TOOL_BASH,
                    bash_args("sleep 30", true),
                ),
                mpsc::channel(8).0,
                CancellationToken::new(),
            )
            .await;
        assert!(!started.is_error, "{}", started.content);
    }
    let refused = host
        .call(
            request("call-over", TOOL_BASH, bash_args("sleep 30", true)),
            mpsc::channel(8).0,
            CancellationToken::new(),
        )
        .await;
    assert!(refused.is_error);
    assert_eq!(
        refused.content,
        "Background job limit reached (15). Wait for running jobs to finish or cancel one."
    );
    host.cancel_all_jobs();
}

#[tokio::test]
async fn abort_jobs_cancels_the_conversations_running_jobs() {
    let (host, mut settlements, _dir) = host_with_settlements();
    host.call(
        request("call-7", TOOL_BASH, bash_args("sleep 30", true)),
        mpsc::channel(8).0,
        CancellationToken::new(),
    )
    .await;
    host.abort_jobs("conversation-1");
    let settled = next_settlement(&mut settlements).await;
    assert_eq!(settled.state.as_str(), "cancelled");
}
