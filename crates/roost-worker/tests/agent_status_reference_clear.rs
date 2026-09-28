//! The worker-authored conversation-reference clear: when the omp process
//! leaves a still-live session the detector appends exactly one durable
//! `reference: null`, so a later restore cannot type a resume for a
//! conversation the user already ended. Drives the detector against a scripted
//! scanner and a recording event sink. Ports v2
//! `apps/worker/tests/agents/agent-status-reference-clear.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_status_support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_status_support::{DetectorHarness, SESSION_ID, detector_harness, session};
use roost_protocol::wire::event::SessionEvent;
use roost_worker::agents::BuiltinAgentId as Agent;
use roost_worker::agents::detector::AgentReferenceClearDeps;
use roost_worker::agents::reference_admission::AgentReferenceAdmissionGate;
use roost_worker::event_store::{DurableEventKind, Reservation, Store};
use roost_worker::session::sinks::{EventFuture, SessionEventError, SessionEventSink};

/// The durable boundary over a real store, recording what it published and
/// how many claims were handed back.
#[derive(Default)]
struct RecordingSink {
    store: Mutex<Store>,
    emitted: Mutex<Vec<SessionEvent>>,
    releases: AtomicUsize,
}

impl SessionEventSink for RecordingSink {
    fn reserve(
        &self,
        kind: DurableEventKind,
    ) -> EventFuture<'_, Result<Reservation, SessionEventError>> {
        let reserved = self
            .store
            .lock()
            .unwrap()
            .reserve_default(kind)
            .map_err(SessionEventError::Reserve);
        Box::pin(std::future::ready(reserved))
    }
    fn hold(&self, reservation: Reservation) -> EventFuture<'_, ()> {
        self.store.lock().unwrap().hold(reservation).unwrap();
        Box::pin(std::future::ready(()))
    }
    fn release(&self, reservation: Reservation) -> EventFuture<'_, ()> {
        self.releases.fetch_add(1, Ordering::SeqCst);
        let _ = self.store.lock().unwrap().release(reservation);
        Box::pin(std::future::ready(()))
    }
    fn emit<'a>(
        &'a self,
        event: &'a SessionEvent,
        reservation: Option<Reservation>,
    ) -> EventFuture<'a, Result<(), SessionEventError>> {
        if let Some(reservation) = reservation {
            let bytes = serde_json::to_vec(event).unwrap().len();
            self.store
                .lock()
                .unwrap()
                .append(reservation, DurableEventKind::AgentReference, bytes)
                .unwrap();
        }
        self.emitted.lock().unwrap().push(event.clone());
        Box::pin(std::future::ready(Ok(())))
    }
}

fn harness() -> (DetectorHarness, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    let harness = detector_harness(Some(AgentReferenceClearDeps {
        event_sink: Arc::clone(&sink) as Arc<dyn SessionEventSink>,
        reference_admission: AgentReferenceAdmissionGate::new(),
    }));
    harness.scanner.set(SESSION_ID, Agent::Omp, 4_321);
    harness.sessions.add("");
    (harness, sink)
}

/// Two passes, then long enough for the spawned clear to take the admission
/// gate's turn and append.
async fn scan_twice(harness: &DetectorHarness) {
    harness.detector.scan_now().await;
    harness.detector.scan_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
}

fn clears(sink: &RecordingSink) -> Vec<SessionEvent> {
    sink.emitted.lock().unwrap().clone()
}

#[tokio::test]
async fn an_omp_process_leaving_a_live_session_clears_the_reference_exactly_once() {
    let (harness, sink) = harness();
    scan_twice(&harness).await;
    assert!(clears(&sink).is_empty());

    harness.scanner.remove(SESSION_ID);
    scan_twice(&harness).await;
    let emitted = clears(&sink);
    assert_eq!(emitted.len(), 1);
    let SessionEvent::AgentReference {
        session_id,
        reference,
        ..
    } = &emitted[0]
    else {
        panic!(
            "the clear is an agent_reference event, got {:?}",
            emitted[0]
        );
    };
    assert_eq!(*session_id, session(SESSION_ID));
    assert_eq!(*reference, None);

    scan_twice(&harness).await;
    scan_twice(&harness).await;
    assert_eq!(clears(&sink).len(), 1);
    assert_eq!(sink.releases.load(Ordering::SeqCst), 0);
    harness.detector.dispose();
}

#[tokio::test]
async fn a_fresh_agent_on_the_same_session_id_clears_again_when_it_leaves() {
    let (harness, sink) = harness();
    scan_twice(&harness).await;
    harness.scanner.remove(SESSION_ID);
    scan_twice(&harness).await;
    harness.scanner.set(SESSION_ID, Agent::Omp, 5_555);
    scan_twice(&harness).await;
    harness.scanner.remove(SESSION_ID);
    scan_twice(&harness).await;
    let emitted = clears(&sink);
    assert_eq!(emitted.len(), 2);
    assert!(emitted.iter().all(|event| matches!(
        event,
        SessionEvent::AgentReference {
            reference: None,
            ..
        }
    )));
    harness.detector.dispose();
}

#[tokio::test]
async fn a_session_that_never_ran_omp_is_never_cleared() {
    let (harness, sink) = harness();
    harness.scanner.set(SESSION_ID, Agent::Pi, 4_321);
    scan_twice(&harness).await;
    harness.scanner.remove(SESSION_ID);
    scan_twice(&harness).await;
    scan_twice(&harness).await;
    assert!(clears(&sink).is_empty());
    harness.detector.dispose();
}

#[tokio::test]
async fn a_session_that_left_the_manager_is_not_cleared() {
    let (harness, sink) = harness();
    scan_twice(&harness).await;
    harness.sessions.clear();
    harness.scanner.remove(SESSION_ID);
    scan_twice(&harness).await;
    assert!(clears(&sink).is_empty());
    harness.detector.dispose();
}
