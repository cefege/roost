//! The snapshot-repair sender: fire and forget, so it correlates nothing, and
//! an offline worker's refusal is named rather than a bare `false`.
//!
//! Ports `sendTerminalSnapshotRequest` of `apps/coord/src/workers/worker-send.ts`.
//! `unwrap`/`expect` are denied outside `#[cfg(test)]`; an integration test is
//! its own crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod workers_send_support;

use roost_coord::workers::send::{SendOutcome, SendRefusal};
use roost_coord::workers::terminal_send::send_terminal_snapshot_request;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use workers_send_support::{TerminalLink, session, worker};

// v2 sendTerminalSnapshotRequest: fire and forget, no correlation entry.
#[tokio::test]
async fn a_snapshot_repair_is_written_without_a_correlation_entry() {
    let link = TerminalLink::routable();
    let outcome =
        send_terminal_snapshot_request(link.relay.workers(), &worker(), &session(), "stream-1");
    assert!(outcome.is_admitted());
    assert_eq!(link.pending_count(), 0);
    let frames = link.frames();
    let [CoordWorkerDownstream::TerminalSnapshotRequest(sent)] = frames.as_slice() else {
        panic!("exactly one snapshot request, got {frames:?}");
    };
    assert_eq!(
        (sent.session_id.as_str(), sent.stream_id.as_str()),
        (session().as_str(), "stream-1")
    );
}

// v2 returns false for an offline worker; here the refusal is named.
#[tokio::test]
async fn a_snapshot_repair_for_an_offline_worker_is_refused_by_name() {
    let link = TerminalLink::offline();
    let outcome =
        send_terminal_snapshot_request(link.relay.workers(), &worker(), &session(), "stream-1");
    assert_eq!(
        outcome,
        SendOutcome::Refused(SendRefusal::NoRoutableGeneration {
            worker_fp: worker()
        })
    );
}
