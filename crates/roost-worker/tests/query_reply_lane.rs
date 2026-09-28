//! The reply lane's write-back: probes a live session's output carried come
//! back on that session's PTY input side, natives and synthesized replies in
//! probe order, chunk after chunk — and one session's slow write never lets
//! its next batch overtake it, nor holds up another session.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod session_support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use roost_protocol::wire::brand::SessionId;
use roost_worker::session::input_write::WorkerInputResult;
use roost_worker::session::query_reply::{PRIMARY_DA_REPLY, QueryReplyLane, WorkerOwnedInput};
use roost_worker::session::scrollback::answer_terminal_queries;
use roost_worker::uplink::OwnerFuture;
use tokio::sync::Semaphore;

use session_support::{Harness, OTHER, SESSION, session_id};

/// A PTY whose writes each wait for their session's gate to be opened.
#[derive(Default)]
struct GatedInput {
    started: Mutex<Vec<(SessionId, Vec<u8>)>>,
    gates: Mutex<HashMap<SessionId, Arc<Semaphore>>>,
}

impl GatedInput {
    fn gate(&self, session: &SessionId) -> Arc<Semaphore> {
        let mut gates = self.gates.lock().expect("held");
        Arc::clone(
            gates
                .entry(session.clone())
                .or_insert_with(|| Arc::new(Semaphore::new(0))),
        )
    }

    fn started(&self) -> Vec<(SessionId, Vec<u8>)> {
        self.started.lock().expect("held").clone()
    }
}

impl WorkerOwnedInput for GatedInput {
    fn write_worker_owned_input(
        &self,
        session_id: &SessionId,
        bytes: Vec<u8>,
    ) -> OwnerFuture<WorkerInputResult> {
        self.started
            .lock()
            .expect("held")
            .push((session_id.clone(), bytes.clone()));
        let gate = self.gate(session_id);
        Box::pin(async move {
            gate.acquire().await.expect("never closed").forget();
            WorkerInputResult::Accepted {
                written_bytes: bytes.len() as u32,
            }
        })
    }
}

async fn settle_until(input: &GatedInput, started: usize) {
    for _ in 0..200 {
        if input.started().len() >= started {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!(
        "only {} writes started, expected {started}",
        input.started().len()
    );
}

#[tokio::test]
async fn a_sessions_next_batch_waits_for_its_previous_write() {
    let input = Arc::new(GatedInput::default());
    let (lane, writer) = QueryReplyLane::new();
    let first = session_id(SESSION);
    let other = session_id(OTHER);
    lane.send(&first, b"A".to_vec());
    lane.send(&first, b"B".to_vec());
    lane.send(&other, b"C".to_vec());
    let running = tokio::spawn(writer.run(Arc::clone(&input)));

    settle_until(&input, 2).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let before = input.started();
    assert_eq!(
        before.len(),
        2,
        "B must not start while A is unanswered: {before:?}"
    );
    assert!(before.contains(&(first.clone(), b"A".to_vec())));
    assert!(
        before.contains(&(other.clone(), b"C".to_vec())),
        "another session is not held up by the first one's write"
    );

    input.gate(&first).add_permits(1);
    settle_until(&input, 3).await;
    assert_eq!(input.started()[2], (first.clone(), b"B".to_vec()));

    input.gate(&first).add_permits(1);
    input.gate(&other).add_permits(1);
    drop(lane);
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("the writer stops once every lane is dropped and its backlog is written")
        .expect("the writer does not panic");
}

/// End to end on a live session: a DA probe and a cursor report in its output
/// come back on its PTY input side in probe order, and a later chunk's reply
/// follows the earlier chunk's.
#[tokio::test]
async fn a_live_sessions_probes_are_answered_on_its_pty_in_probe_order() {
    let harness = Harness::new();
    harness.install(SESSION, 5, "/", "/");
    let (lane, writer) = QueryReplyLane::new();
    let running = tokio::spawn(writer.run(Arc::clone(&harness.manager)));
    harness
        .table
        .with_record_mut(&session_id(SESSION), |record| {
            answer_terminal_queries(record, b"ab\x1b[c\x1b[6n", &lane);
            answer_terminal_queries(record, b"\x1b[6n", &lane);
        })
        .expect("the session is live");
    drop(lane);
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("the writer drains and stops")
        .expect("the writer does not panic");
    assert_eq!(
        harness.keeper.input.written(),
        vec![
            (5, format!("{PRIMARY_DA_REPLY}\x1b[1;3R").into_bytes()),
            (5, b"\x1b[1;3R".to_vec()),
        ]
    );
}
