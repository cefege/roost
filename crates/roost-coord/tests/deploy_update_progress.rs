//! A worker's `update-progress` frame on the link: refused when it names no job
//! or a sequence past the safe-integer range, and handled on a socket that has
//! not crossed its snapshot barrier, because progress settles no pending RPC.
//!
//! Pins the `updateProgress` arm of `apps/coord/src/workers/worker-frame-dispatch.ts`.
//!
//! `unwrap` and `expect` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to be
//! stated here. Every panic names a value the test just built.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod frame_dispatch_support;
mod workers_support;

use frame_dispatch_support::{LinkFixture, WORKER_FP, rpc_frame};
use roost_coord::worker_link::dispatch::{DispatchOutcome, FrameDispatch};
use roost_protocol::wire::coord_worker::{CoordWorkerUpstream, UpdateProgress};

fn progress(job_id: &str, sequence: u64) -> CoordWorkerUpstream {
    CoordWorkerUpstream::UpdateProgress(UpdateProgress {
        request_id: "req-1".to_owned(),
        job_id: job_id.to_owned(),
        sequence,
        phase: "download".to_owned(),
        message: "fetching".to_owned(),
        terminal: false,
        success: false,
        error: String::new(),
    })
}

#[tokio::test]
async fn progress_naming_no_job_or_an_unsafe_sequence_is_refused() {
    let fixture = LinkFixture::new("update-progress-invalid").await;
    fixture.mark_ready();
    let dispatcher = fixture.dispatcher();
    let job_id = "00000000-0000-4000-8000-000000000001";
    for frame in [progress("", 1), progress(job_id, 1 << 53)] {
        assert_eq!(
            dispatcher.handle_now(WORKER_FP, rpc_frame(frame)),
            DispatchOutcome::Refused
        );
    }
    assert_eq!(
        dispatcher.handle_now(WORKER_FP, rpc_frame(progress(job_id, (1 << 53) - 1))),
        DispatchOutcome::Handled
    );
}

#[tokio::test]
async fn progress_before_the_snapshot_barrier_is_handled_not_dropped() {
    let fixture = LinkFixture::new("update-progress-unready").await;
    let dispatcher = fixture.dispatcher();
    let job = fixture
        .services
        .deploy
        .journal()
        .open_job("m1-us.tailnet.ts.net")
        .unwrap();
    assert_eq!(
        dispatcher.handle_now(WORKER_FP, rpc_frame(progress(job.job_id(), 1))),
        DispatchOutcome::Handled
    );
}
