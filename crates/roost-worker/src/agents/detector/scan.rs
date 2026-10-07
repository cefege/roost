//! One detection pass: the live sessions, their agent processes, and for each
//! identified agent a rate-limited read of its grid judged by its manifest and
//! stabilised before it reaches the registry. Ports v2 `detector.ts`
//! `scanOnce`. Run only by the detector's own scan task.

use std::collections::HashSet;

use roost_protocol::wire::brand::SessionId;

use super::super::manifest_engine::{DetectionInput, evaluate_manifest};
use super::super::process_scan::{AgentProcessIdentity, SessionProcessRoot};
use super::{AgentScreenDetector, ObservedAgentIdentity, SCREEN_RESCAN_MIN_MS, lock};

impl AgentScreenDetector {
    pub(super) async fn scan_once(&self) {
        let deps = &self.inner.deps;
        let records = deps.sessions.all_sessions();
        let live: HashSet<SessionId> = records
            .iter()
            .map(|record| record.session_id.clone())
            .collect();
        lock(&self.inner.stable).retain(&live);
        deps.registry.retain_sessions(&live);
        {
            let mut state = lock(&self.inner.state);
            state
                .last_screen_read_at
                .retain(|session_id, _| live.contains(session_id));
            state
                .last_observed
                .retain(|session_id, _| live.contains(session_id));
            state.channel_by_session = records
                .iter()
                .map(|record| (record.session_id.clone(), record.channel_id))
                .collect();
        }
        let roots: Vec<SessionProcessRoot> = records
            .iter()
            .filter_map(|record| {
                let child_pid = record.child_pid.filter(|pid| *pid > 0)?;
                Some(SessionProcessRoot {
                    session_id: record.session_id.clone(),
                    child_pid,
                })
            })
            .collect();
        let identities = deps.scanner.scan_agents(roots).await;
        for record in &records {
            let session_id = &record.session_id;
            match identities.get(session_id) {
                None => {
                    lock(&self.inner.state)
                        .last_screen_read_at
                        .remove(session_id);
                    lock(&self.inner.stable).release(session_id);
                    deps.registry.clear_screen(session_id);
                }
                Some(identity) => self.observe_screen(session_id, identity),
            }
        }
    }

    /// One identified session: clear OSC evidence on an agent change, then
    /// read and judge the grid unless it was read within the rescan gap.
    fn observe_screen(&self, session_id: &SessionId, identity: &AgentProcessIdentity) {
        let deps = &self.inner.deps;
        let observed = ObservedAgentIdentity {
            agent_id: identity.agent_id,
            process_id: identity.pid,
        };
        let previous = lock(&self.inner.state)
            .last_observed
            .insert(session_id.clone(), observed);
        if let Some(previous) = previous.filter(|previous| *previous != observed) {
            deps.sessions.clear_osc_evidence(session_id);
            tracing::info!(
                session_id = %session_id,
                previous_agent_id = previous.agent_id.as_str(),
                agent_id = identity.agent_id.as_str(),
                "osc_evidence_cleared_on_agent_change"
            );
        }
        let now_ms = i64::try_from(deps.clock.mono_ns() / 1_000_000).unwrap_or(i64::MAX);
        {
            let mut state = lock(&self.inner.state);
            let last_read = state.last_screen_read_at.get(session_id);
            if last_read.is_some_and(|last_read| now_ms - last_read < SCREEN_RESCAN_MIN_MS) {
                return;
            }
            state.last_screen_read_at.insert(session_id.clone(), now_ms);
        }
        let Some(evidence) = deps.sessions.screen_evidence(session_id) else {
            tracing::debug!(session_id = %session_id, "a session left before its screen was read");
            return;
        };
        let detection = evaluate_manifest(
            deps.manifests.get(identity.agent_id),
            &DetectionInput {
                screen: &evidence.screen,
                osc_title: &evidence.osc_title,
                osc_progress: &evidence.osc_progress,
            },
        );
        let wall_ms = deps.clock.now_epoch_ms();
        let (report, current) = {
            let mut stable = lock(&self.inner.stable);
            let report = stable.observe(session_id, identity, &detection, wall_ms);
            let current = stable.current(session_id).is_some();
            (report, current)
        };
        match report {
            Some(report) => {
                deps.registry.report_screen(session_id, report);
            }
            None if !current => deps.registry.clear_screen(session_id),
            None => {}
        }
    }
}
