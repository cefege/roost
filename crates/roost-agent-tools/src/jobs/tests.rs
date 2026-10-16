//! Unit tests for the job registry: the running-job cap, slot reuse once a job
//! settles, and the cancelled-to-settled path that reaches the coordinator.

use super::*;

fn registry() -> JobRegistry {
    JobRegistry::new(mpsc::unbounded_channel().0)
}

fn capture() -> Arc<AsyncMutex<BashCapture>> {
    Arc::new(AsyncMutex::new(BashCapture::new(&std::env::temp_dir())))
}

#[test]
fn caps_running_jobs_per_conversation_and_frees_a_slot_on_settle() {
    let registry = registry();
    let mut job_ids = Vec::new();
    for _ in 0..MAX_RUNNING_JOBS {
        job_ids.push(
            registry
                .register("conv", "sleep 1", CancellationToken::new(), capture())
                .expect("under the cap"),
        );
    }
    assert_eq!(job_ids.first().map(String::as_str), Some("bg_1"));
    assert_eq!(
        registry.register("conv", "sleep 1", CancellationToken::new(), capture()),
        Err(JobLimitReached)
    );
    assert!(
        registry
            .register("other", "sleep 1", CancellationToken::new(), capture())
            .is_ok()
    );
    registry.settle("conv", "bg_1", ToolOutcome::success("done"));
    assert!(
        registry
            .register("conv", "sleep 1", CancellationToken::new(), capture())
            .is_ok()
    );
}

#[test]
fn cancelled_running_job_settles_as_cancelled() {
    let (settled_tx, mut settled_rx) = mpsc::unbounded_channel();
    let registry = JobRegistry::new(settled_tx);
    let job_id = registry
        .register("conv", "sleep 9", CancellationToken::new(), capture())
        .expect("registered");
    registry.cancel_running("conv");
    registry.settle("conv", &job_id, ToolOutcome::failure("command cancelled"));
    let settled = settled_rx.try_recv().expect("settled job");
    assert_eq!(settled.state, JobState::Cancelled);
    assert_eq!(settled.job_id, "bg_1");
}
