//! The agent-reference admission gate and the one durable append every
//! reference producer shares: turns run one at a time in the order they were
//! asked for and hand on even when they fail, a set and a clear each consume
//! exactly the claim taken for them, and a failed or refused append gives its
//! claim back. Ports the gate half of
//! `apps/worker/tests/agents/agent-reference-reconcile-gate.test.ts` and the
//! append rules of `apps/worker/src/agents/reference-admission.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod session_support;

use std::sync::{Arc, Mutex};

use roost_protocol::agent_conversation_reference::{
    AgentConversationReferenceKind, AgentConversationReferenceV1,
};
use roost_protocol::wire::event::SessionEvent;
use roost_worker::agents::reference_admission::{
    AgentReferenceAdmissionGate, emit_durable_agent_reference,
};
use roost_worker::session::sinks::SessionEventError;
use session_support::{Harness, SESSION, session_id};
use tokio::sync::oneshot;

fn omp_reference(value: &str) -> AgentConversationReferenceV1 {
    AgentConversationReferenceV1 {
        schema_version: 1,
        agent_id: "omp".to_owned(),
        kind: AgentConversationReferenceKind::Path,
        value: value.to_owned(),
    }
}

#[tokio::test]
async fn a_later_turn_waits_for_the_one_holding_the_gate_and_runs_in_arrival_order() {
    let gate = AgentReferenceAdmissionGate::new();
    let order: Arc<Mutex<Vec<&str>>> = Arc::default();
    let (release, held) = oneshot::channel::<()>();
    let (entered, holding) = oneshot::channel::<()>();
    let first = {
        let (gate, order) = (gate.clone(), Arc::clone(&order));
        tokio::spawn(async move {
            gate.run_exclusive(|| async move {
                order.lock().unwrap().push("reconcile");
                entered.send(()).unwrap();
                held.await.unwrap();
            })
            .await;
        })
    };
    holding.await.unwrap();
    let mut later = Vec::new();
    for name in ["reporter", "clear"] {
        let (gate, order) = (gate.clone(), Arc::clone(&order));
        later.push(tokio::spawn(async move {
            gate.run_exclusive(|| async move { order.lock().unwrap().push(name) }).await;
        }));
        tokio::task::yield_now().await;
    }
    assert_eq!(*order.lock().unwrap(), ["reconcile"], "a reporter entered while reconciliation held the gate");

    release.send(()).unwrap();
    first.await.unwrap();
    for task in later {
        task.await.unwrap();
    }
    assert_eq!(*order.lock().unwrap(), ["reconcile", "reporter", "clear"]);
}

#[tokio::test]
async fn a_failed_turn_still_hands_the_gate_on() {
    let gate = AgentReferenceAdmissionGate::new();
    let failed: Result<(), &str> = gate.run_exclusive(|| async { Err("reporter failed") }).await;
    assert_eq!(failed, Err("reporter failed"));
    assert_eq!(gate.run_exclusive(|| async { 7 }).await, 7);
}

#[tokio::test]
async fn a_set_and_a_clear_each_consume_their_own_claim() {
    let harness = Harness::new();
    let sid = session_id(SESSION);
    let reference = omp_reference("/private/session.jsonl");

    emit_durable_agent_reference(harness.sink.as_ref(), &sid, Some(&reference)).await.unwrap();
    emit_durable_agent_reference(harness.sink.as_ref(), &sid, None).await.unwrap();

    let published = harness.sink.published();
    let references: Vec<_> = published
        .iter()
        .map(|event| match event {
            SessionEvent::AgentReference { session_id, reference, ts, trace_id } => {
                assert_eq!(session_id, &sid);
                assert!(*ts > 0 && trace_id.is_none());
                reference.clone()
            }
            other => panic!("only agent references were appended, got {other:?}"),
        })
        .collect();
    assert_eq!(references, [Some(reference), None]);
    assert_eq!(harness.sink.store.lock().unwrap().live_reservations(), 0, "a claim outlived its append");
}

#[tokio::test]
async fn a_failed_append_gives_its_claim_back() {
    let harness = Harness::new();
    *harness.sink.fail_next.lock().unwrap() = true;
    let failed = emit_durable_agent_reference(harness.sink.as_ref(), &session_id(SESSION), None).await;
    assert!(matches!(failed, Err(SessionEventError::Unclassifiable(_))), "{failed:?}");
    assert!(harness.sink.published().is_empty());
    assert_eq!(harness.sink.store.lock().unwrap().live_reservations(), 0, "the failed append leaked its claim");
}

#[tokio::test]
async fn a_reference_the_event_schema_refuses_never_reaches_the_outbox() {
    let harness = Harness::new();
    let relative = omp_reference("relative/session.jsonl");
    let refused = emit_durable_agent_reference(harness.sink.as_ref(), &session_id(SESSION), Some(&relative)).await;
    assert!(refused.is_err());
    assert!(harness.sink.published().is_empty());
    assert_eq!(harness.sink.store.lock().unwrap().live_reservations(), 0, "the refused append leaked its claim");
}
