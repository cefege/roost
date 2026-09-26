//! The disconnect-is-not-a-shutdown rule, proven against a real socket.
//!
//! Why the slices' own tests cannot cover this: `tests/link_barrier.rs` proves
//! what the barrier releases, `tests/backoff_policy.rs` proves the delays, and
//! `tests/link_dial.rs` proves one dial's lifecycle. None of them run a loop, so
//! none of them can catch a loop that treats a dropped coordinator link as a
//! reason to end the process — which would take every PTY the keeper is holding
//! down with it, on every coordinator restart, fleet-wide.
//!
//! The live test is the point. It stands up a coordinator that reads the forced
//! hello and then closes the way a restarting one does, and asserts the worker
//! dials again rather than returning.

// A test unwraps the value it is asserting about: a failure there IS the
// assertion failing, which is what a test wants. The workspace denies
// unwrap/expect because a panic on a bad wire value in a running component is a
// fleet-visible outage, and that reasoning does not reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};
use roost_worker::link_dial::CoordinatorEndpoint;
use roost_worker::runtime::credential::{CredentialError, CredentialSource};
use roost_worker::runtime::link_loop::{LinkLoop, WorkerIdentity};
use roost_worker::runtime::link_wire::{LinkWire, WireError};
use roost_worker::runtime::snapshot_source::{NoSnapshot, SnapshotSource};
use roost_worker::runtime::stop::{
    LinkEnd, LinkEndOutcome, StopReason, StopRequests, verdict_for_link_end,
};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

const FINGERPRINT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Long enough that a slow machine does not fail the test, short enough that a
/// genuine hang is a failure rather than a stall.
const PATIENCE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy)]
struct FixedCredential;

impl CredentialSource for FixedCredential {
    fn mint(&self) -> Result<String, CredentialError> {
        Ok("a-test-credential".to_string())
    }
}

/// A codec that speaks the frame's own name as its bytes.
///
/// The ported production codec does not exist yet, and a test that asserted on
/// protobuf bytes would be testing a codec rather than the loop. What the loop
/// owes this test is that it force-writes something as its first frame and then
/// keeps serving; what the name carries is enough to prove that.
#[derive(Debug, Clone, Copy)]
struct NameCodec;

impl LinkWire for NameCodec {
    fn encode_upstream(&self, frame: &CoordWorkerUpstream) -> Result<Vec<u8>, WireError> {
        Ok(frame.kind().as_bytes().to_vec())
    }

    fn decode_downstream(&self, bytes: &[u8]) -> Result<CoordWorkerDownstream, WireError> {
        match bytes {
            b"hello-ack" => Ok(CoordWorkerDownstream::HelloAck {
                capabilities: Vec::new(),
                trace_id: None,
            }),
            other => Err(WireError::Undecodable {
                reason: format!("this test's server never sends {other:?}"),
            }),
        }
    }
}

/// A coordinator that reads the forced hello and then closes the way a
/// restarting one does: cleanly, with no error, mid-conversation.
async fn read_the_hello_then_close(listener: TcpListener, opened: mpsc::UnboundedSender<()>) {
    while let Ok((stream, _)) = listener.accept().await {
        let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
            continue;
        };
        let _ = socket.next().await;
        let _ = socket.close(None).await;
        let _ = opened.send(());
    }
}

/// Every network condition is an ordinary reconnect. The only way to end the
/// process is to carry a reason somebody already asked for, and this is the
/// function that says so.
#[test]
fn no_link_ending_stops_the_worker_and_only_a_requested_reason_does() {
    let silent = Duration::from_secs(120);
    for end in [
        LinkEnd::Closed,
        LinkEnd::FrameError("a frame did not decode".to_string()),
        LinkEnd::Stale { silent },
        LinkEnd::WriteFailed("the socket went away mid-write".to_string()),
        LinkEnd::HelloFailed("the hello did not encode".to_string()),
        LinkEnd::SnapshotStarved { waited: silent },
    ] {
        assert_eq!(
            verdict_for_link_end(&end),
            LinkEndOutcome::Redial,
            "{end:?} is a network condition, and a worker that ended on one would \
             take every PTY its keeper is holding down with it"
        );
    }
    assert_eq!(
        verdict_for_link_end(&LinkEnd::Stopped(StopReason::Signal("SIGTERM"))),
        LinkEndOutcome::Stop(StopReason::Signal("SIGTERM"))
    );
    assert_eq!(
        verdict_for_link_end(&LinkEnd::Stopped(StopReason::ShutdownFrame)),
        LinkEndOutcome::Stop(StopReason::ShutdownFrame)
    );
}

/// The first reason wins, so an impatient second `SIGTERM` does not run the
/// teardown twice — the failure the sibling daemon has already had once.
#[test]
fn the_first_stop_request_is_the_one_that_stands() {
    let (requests, signal) = StopRequests::channel();
    assert!(!requests.is_requested());
    assert!(signal.reason().is_none());

    assert!(requests.request(StopReason::Signal("SIGTERM")));
    assert!(requests.is_requested());
    assert_eq!(signal.reason(), Some(StopReason::Signal("SIGTERM")));

    assert!(
        !requests.request(StopReason::Signal("SIGINT")),
        "a second signal is ignored rather than escalating, because the teardown \
         is already running"
    );
    assert_eq!(
        signal.reason(),
        Some(StopReason::Signal("SIGTERM")),
        "and the reason the log reports is the one the first request carried"
    );
}

/// The live proof. A coordinator that drops the link is a reconnect, and the
/// worker is still running afterwards; only a requested stop ends it.
#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_link_is_a_reconnect_and_a_stop_is_a_stop() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port");
    let address = listener.local_addr().expect("the bound loopback address");
    let (opened_tx, mut opened_rx) = mpsc::unbounded_channel();
    let coordinator = tokio::spawn(read_the_hello_then_close(listener, opened_tx));

    let (requests, stop) = StopRequests::channel();
    let endpoint = CoordinatorEndpoint::new(format!("http://{address}"), FINGERPRINT)
        .expect("a loopback endpoint is a usable endpoint");
    let worker = tokio::spawn(
        LinkLoop::new(
            endpoint,
            WorkerIdentity {
                worker_fp: WorkerFp::try_from(FINGERPRINT).expect("a well-shaped fingerprint"),
                version: "test".to_string(),
                process_epoch: "test-epoch".to_string(),
            },
            Arc::new(NameCodec),
            Arc::new(NoSnapshot),
            Arc::new(FixedCredential),
        )
        .run(stop),
    );

    tokio::time::timeout(PATIENCE, opened_rx.recv())
        .await
        .expect("the worker dialled at all")
        .expect("the coordinator reported the link");
    assert!(
        !requests.is_requested(),
        "a coordinator the worker could not keep a link to is not a stop request"
    );

    tokio::time::timeout(PATIENCE, opened_rx.recv())
        .await
        .expect("a dropped link produced a second dial")
        .expect("the coordinator reported the second link");
    assert!(
        !worker.is_finished(),
        "the worker returned after a dropped link, which is the one thing this \
         rule exists to prevent: the keeper is still serving every PTY on this \
         machine and a worker that exits takes nothing — but a worker that \
         exits on a coordinator restart is a worker that will be restarted by \
         every coordinator restart"
    );
    assert!(!requests.is_requested());

    assert!(requests.request(StopReason::ShutdownFrame));
    let reason = tokio::time::timeout(PATIENCE, worker)
        .await
        .expect("the run ends once a stop is requested")
        .expect("the run task did not panic");
    assert_eq!(reason, StopReason::ShutdownFrame);
    coordinator.abort();
}

/// A snapshot source that cannot answer is not the same as one that answers with
/// nothing, and the loop has to be able to tell the difference.
#[test]
fn a_source_with_no_sessions_is_not_a_source_with_no_snapshot() {
    let inactive: &dyn SnapshotSource = &NoSnapshot;
    assert!(
        !inactive.is_active(),
        "an inactive source is what the link reports at boot, so the missing \
         snapshot is one log line rather than one per dial"
    );
    assert!(inactive.snapshot().is_err());
}
