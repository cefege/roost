//! Unit tests for the `bash` tool: streaming, exit status, timeout, cancellation,
//! spill disclosure, and the explicit, auto-background and steer detach paths.

use std::path::Path;

use tempfile::tempdir;

use super::*;

fn bash_call(directory: &Path, jobs: &Arc<JobRegistry>, script: &str, timeout_ms: u32) -> BashCall {
    BashCall {
        data_dir: directory.to_path_buf(),
        cwd: directory.to_path_buf(),
        conversation_id: "conv".to_owned(),
        args: BashArgs {
            command: script.to_owned(),
            timeout: None,
            background: None,
        },
        timeout_ms,
        auto_background_after: DEFAULT_AUTO_BACKGROUND_AFTER,
        detach: Arc::new(Notify::new()),
        jobs: jobs.clone(),
    }
}

async fn execute(
    script: &str,
    timeout_ms: u32,
    cancel: CancellationToken,
    out: &mpsc::Sender<String>,
) -> ToolOutcome {
    let directory = tempdir().expect("tempdir");
    let jobs = Arc::new(JobRegistry::new(mpsc::unbounded_channel().0));
    bash_tool(
        bash_call(directory.path(), &jobs, script, timeout_ms),
        out,
        cancel,
    )
    .await
}

fn details(outcome: &ToolOutcome) -> serde_json::Value {
    serde_json::from_str(&outcome.details_json).expect("details json")
}

#[tokio::test]
async fn streams_output_and_preserves_nonzero_exit_status() {
    let (out, mut chunks) = mpsc::channel(64);
    let result = execute("printf streamed; exit 7", 0, CancellationToken::new(), &out).await;
    let mut streamed = String::new();
    while let Ok(chunk) = chunks.try_recv() {
        streamed.push_str(&chunk);
    }
    assert!(result.is_error);
    assert_eq!(details(&result)["exit_code"], 7);
    assert!(streamed.contains("streamed"));
}

#[tokio::test]
async fn enforces_timeout() {
    let (out, _chunks) = mpsc::channel(64);
    let result = execute("sleep 5", 50, CancellationToken::new(), &out).await;
    assert!(result.is_error);
    assert_eq!(details(&result)["timed_out"], true);
    assert_eq!(details(&result)["cancelled"], false);
}

#[tokio::test]
async fn cancellation_terminates_command() {
    let (out, _chunks) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        trigger.cancel();
    });
    let result = execute("sleep 5", 0, cancel, &out).await;
    assert!(result.is_error);
    assert_eq!(details(&result)["cancelled"], true);
    assert_eq!(details(&result)["timed_out"], false);
}

#[tokio::test]
async fn spilled_prefix_is_not_advertised_as_complete_output() {
    let directory = tempdir().expect("tempdir");
    let jobs = Arc::new(JobRegistry::new(mpsc::unbounded_channel().0));
    let (out, _chunks) = mpsc::channel(64);
    let result = bash_tool(
        bash_call(
            directory.path(),
            &jobs,
            "printf '%60000s' ''; printf tail-marker",
            0,
        ),
        &out,
        CancellationToken::new(),
    )
    .await;
    let details = details(&result);
    assert_eq!(details["truncated"], true);
    assert_eq!(details["artifact_complete"], false);
    let artifact = std::fs::read_to_string(details["artifact_path"].as_str().expect("path"))
        .expect("artifact file");
    assert!(!artifact.contains("tail-marker"));
    assert!(result.content.contains("tail-marker"));
}

#[tokio::test]
async fn explicit_async_call_returns_running_job_and_settles_completed() {
    let directory = tempdir().expect("tempdir");
    let (settled_tx, mut settled_rx) = mpsc::unbounded_channel();
    let jobs = Arc::new(JobRegistry::new(settled_tx));
    let (out, _chunks) = mpsc::channel(64);
    let mut call = bash_call(directory.path(), &jobs, "sleep 0.2; printf finished", 0);
    call.args.background = Some(true);

    let started = bash_tool(call, &out, CancellationToken::new()).await;

    assert!(!started.is_error);
    assert_eq!(details(&started)["async"]["state"], "running");
    assert_eq!(details(&started)["async"]["job_id"], "bg_1");
    let settled = tokio::time::timeout(Duration::from_secs(20), settled_rx.recv())
        .await
        .expect("job settles")
        .expect("settled channel open");
    assert_eq!(settled.job_id, "bg_1");
    assert_eq!(settled.state, JobState::Completed);
    assert_eq!(settled.output, "finished");
}

#[tokio::test]
async fn auto_background_detaches_a_call_that_outlives_the_threshold() {
    let directory = tempdir().expect("tempdir");
    let (settled_tx, mut settled_rx) = mpsc::unbounded_channel();
    let jobs = Arc::new(JobRegistry::new(settled_tx));
    let (out, _chunks) = mpsc::channel(64);
    let mut call = bash_call(directory.path(), &jobs, "sleep 0.3; printf finished", 0);
    call.auto_background_after = Duration::from_millis(50);

    let started = bash_tool(call, &out, CancellationToken::new()).await;

    assert!(!started.is_error);
    assert!(started.content.starts_with("Backgrounded as job bg_1"));
    assert_eq!(details(&started)["async"]["state"], "running");
    let settled = tokio::time::timeout(Duration::from_secs(20), settled_rx.recv())
        .await
        .expect("job settles")
        .expect("settled channel open");
    assert_eq!(settled.state, JobState::Completed);
    assert_eq!(settled.output, "finished");
}

#[tokio::test]
async fn steer_detaches_the_running_call_and_says_so() {
    let directory = tempdir().expect("tempdir");
    let (settled_tx, mut settled_rx) = mpsc::unbounded_channel();
    let jobs = Arc::new(JobRegistry::new(settled_tx));
    let (out, _chunks) = mpsc::channel(64);
    let call = bash_call(directory.path(), &jobs, "sleep 0.3; printf finished", 0);
    let detach = call.detach.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        detach.notify_one();
    });

    let started = bash_tool(call, &out, CancellationToken::new()).await;

    assert!(started.content.starts_with(
        "Backgrounded early to handle an incoming message; the command keeps running."
    ));
    assert_eq!(details(&started)["async"]["state"], "running");
    let settled = tokio::time::timeout(Duration::from_secs(20), settled_rx.recv())
        .await
        .expect("job settles")
        .expect("settled channel open");
    assert_eq!(settled.state, JobState::Completed);
}

#[test]
fn auto_background_threshold_is_capped_one_second_short_of_the_deadline() {
    assert_eq!(
        auto_background_threshold(Duration::from_secs(60), Duration::from_secs(120)),
        Some(Duration::from_secs(60))
    );
    assert_eq!(
        auto_background_threshold(Duration::from_secs(60), Duration::from_secs(5)),
        Some(Duration::from_secs(4))
    );
    assert_eq!(
        auto_background_threshold(Duration::from_secs(60), Duration::from_secs(1)),
        None
    );
}
