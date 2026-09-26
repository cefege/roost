//! Session projection: one fold, delegated, never mirrored.
//!
//! `docs/FAILURE-INDEX.md:27` — "SPA projector hand-mirrors the shared event
//! fold" — is why this file has no `match` in it. v2's hand-mirrored switch
//! drifted and silently dropped the `respawned` variant, so the coordinator's
//! database projection and the browser's in-memory projection disagreed about a
//! live session. The fix there was delegation; the fix here is to have nothing
//! to delegate FROM.
//!
//! Depends on `roost_protocol::wire`, and owns nothing else.

use roost_protocol::wire::{Session, SessionEvent, SessionMap, fold_all, fold_event};

/// The durable public session event, as the host decoded it.
///
/// A newtype over `roost_protocol::wire::SessionEvent` rather than a
/// hand-written mirror of it. Private and viewer-local variants (`attached`,
/// `detached`, `agent_reference`) do not appear as members here: they are
/// durable and ordered but not public session state, and the shared fold already
/// treats them as explicit no-ops.
#[derive(Debug, Clone, PartialEq)]
pub struct WireEvent(pub SessionEvent);

/// An authoritative session row.
pub type WireSession = Session;

/// The projected session plane.
///
/// Every mutating method is a thin wrapper over the shared fold. They exist so a
/// caller reads as "apply this event" rather than "call `fold_event` and
/// remember to reassign", because a wrapper that forgets the reassignment is
/// exactly the defect this module was written to make impossible.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionPlane {
    sessions: SessionMap,
}

impl SessionPlane {
    /// An empty projection.
    pub fn new() -> Self {
        Self::default()
    }

    /// The projection, for a host that renders it.
    pub fn sessions(&self) -> &SessionMap {
        &self.sessions
    }

    /// One session by id.
    pub fn session(&self, session_id: &roost_protocol::wire::SessionId) -> Option<&Session> {
        self.sessions.get(session_id)
    }

    /// How many sessions are projected.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether the projection is empty.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Fold one event.
    ///
    /// `fold_event` never mutates its input and returns the whole next map, so
    /// the assignment IS the projection. Only `SessionEvent::Closed` removes a
    /// session, which is what makes a stale or out-of-order event harmless: it
    /// cannot prune a live session as a side effect. (v2 needed an explicit
    /// "only `closed` deletes" branch for the same reason —
    /// `apps/web/src/store/projector.ts:125-133`.)
    pub fn apply(&mut self, event: &SessionEvent) {
        self.sessions = fold_event(&self.sessions, event);
    }

    /// Replace the projection with an authoritative full set.
    ///
    /// Bootstrap, and every re-hydration a reconnect's fresh domain generation
    /// triggers. This is the ONLY path that prunes: an absent session in a stale
    /// or reordered EVENT is an offline breadcrumb, not a deletion, so it must
    /// not unwind a live session — which is why a snapshot and a fold are
    /// different methods rather than one method with a flag.
    pub fn apply_snapshot(&mut self, sessions: SessionMap) {
        self.sessions = sessions;
    }

    /// Fold a whole ordered batch in one pass. What the projection-agreement
    /// guard compares against.
    pub fn apply_all(&mut self, events: &[SessionEvent]) {
        self.sessions = fold_all(events);
    }
}

#[cfg(test)]
mod tests {
    use roost_protocol::wire::{ChannelId, SessionId, SessionKind, WorkerFp};

    use super::SessionPlane;

    const SESSION_A: &str = "00000000-0000-4000-8000-000000000abc";
    const SESSION_B: &str = "00000000-0000-4000-8000-000000000abd";
    const WORKER_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn opened(session_id: &str) -> roost_protocol::wire::SessionEvent {
        roost_protocol::wire::SessionEvent::Opened {
            // A test unwrap fails the assertion, which is the point of the test.
            session_id: SessionId::try_from(session_id).expect("a valid session id"),
            worker_fp: WorkerFp::try_from(WORKER_FP).expect("a valid fingerprint"),
            channel: ChannelId::try_from(0_i64).expect("a valid channel"),
            session_kind: SessionKind::Shell,
            cwd: "/repo".to_owned(),
            ts: 1,
            trace_id: None,
        }
    }

    #[test]
    fn the_projection_is_the_shared_fold() {
        // The guard `docs/FAILURE-INDEX.md:27` names, in Rust: the client's
        // projection is reference-equal to `fold_all` over the same events.
        let events = [opened(SESSION_A), opened(SESSION_B)];
        let mut plane = SessionPlane::new();
        plane.apply_all(&events);
        assert_eq!(plane.sessions(), &roost_protocol::wire::fold_all(&events));
    }

    #[test]
    fn an_incremental_fold_equals_a_batch_fold() {
        // The same property the TypeScript property test checked, ported.
        let events = [opened(SESSION_A), opened(SESSION_B)];
        let mut incremental = SessionPlane::new();
        for event in &events {
            incremental.apply(event);
        }
        let mut batch = SessionPlane::new();
        batch.apply_all(&events);
        assert_eq!(incremental.sessions(), batch.sessions());
    }

    #[test]
    fn only_a_closed_event_removes_a_session() {
        let mut plane = SessionPlane::new();
        plane.apply(&opened(SESSION_A));
        let opened_id = SessionId::try_from(SESSION_A).expect("a valid session id");
        assert_eq!(plane.len(), 1);
        plane.apply(&roost_protocol::wire::SessionEvent::Closed {
            session_id: opened_id.clone(),
            exit_code: Some(0),
            ts: 2,
            trace_id: None,
        });
        assert!(plane.session(&opened_id).is_none());
        assert!(plane.is_empty());
    }
}
