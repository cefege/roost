//! The one transition over a session's occupancy: from its live candidates to
//! the effective occupant, publishing the frames that edge implies. Ports v2
//! `AgentStatusRegistry.recompute`, `effectiveFrame`, `retireProcess` and
//! `nextRevision` (`apps/worker/src/agents/registry.ts`). Called only by
//! `agents::registry`, under its state lock, so publication order is revision
//! order.

use roost_protocol::wire::agent_status::{
    AgentId, AgentOccupantId, AgentRuntimeState, AgentStatusFields, AgentStatusSource,
    AgentStatusUpdate, StatusEpoch,
};
use roost_protocol::wire::brand::SessionId;

use super::BuiltinAgentId;
use super::registry::{AgentStatusPublisher, RegistryState};
use crate::agent_occupancy::{CandidateLoss, EffectiveEntry, ProcessKey, SessionEntry};
use crate::session::ids::mint_uuid;

/// Agent runtimes whose integration reports cover the whole agent lifecycle,
/// so no screen signal may correct them. Every other reporter proves identity
/// and activity only: a visible blocker prompt outranks its state.
fn full_lifecycle_agent(agent_id: BuiltinAgentId) -> bool {
    matches!(agent_id, BuiltinAgentId::Omp | BuiltinAgentId::Pi)
}

/// v2 `nextRevision`: strictly increasing, and never behind the clock.
fn next_revision(revision: &mut i64, now: i64) -> i64 {
    *revision = (*revision + 1).max(now.saturating_mul(1_000));
    *revision
}

/// v2 `retireProcess`: a retired key can never be reclaimed by a report.
fn retire_process(entry: &mut SessionEntry, retired: ProcessKey) {
    entry.retired_process_keys.insert(retired);
    if entry
        .integration
        .as_ref()
        .is_some_and(|integration| integration.process.process_key == retired)
    {
        entry.integration = None;
    }
    if entry
        .screen
        .as_ref()
        .is_some_and(|screen| screen.process.process_key == retired)
    {
        entry.screen = None;
    }
}

/// v2 `effectiveFrame`. `None` only if the frame fails the protocol's own
/// validation, which v2 would have thrown on; it is logged and not published.
pub(super) fn effective_frame(
    session_id: &SessionId,
    status_epoch: &StatusEpoch,
    effective: &EffectiveEntry,
    active: bool,
    revision: i64,
    updated_at: i64,
) -> Option<AgentStatusUpdate> {
    let agent_id = match AgentId::try_from(effective.process.agent_id.as_str()) {
        Ok(agent_id) => agent_id,
        Err(error) => {
            tracing::error!(%error, "an agent status named an agent id the protocol refuses");
            return None;
        }
    };
    let common = AgentStatusFields {
        session_id: session_id.clone(),
        agent_id,
        state: effective.process.state,
        message: effective.message.clone(),
        revision,
        completed_revision: effective.completed_revision,
        updated_at,
        status_epoch: Some(status_epoch.clone()),
        occupant_id: Some(effective.occupant_id.clone()),
        source: Some(effective.source),
        occupant_exited: !effective.occupant_live,
    };
    if let Err(error) = common.check() {
        tracing::error!(%error, session = %session_id, "an agent status failed validation and was not published");
        return None;
    }
    Some(AgentStatusUpdate { common, active })
}

/// One recompute pass: the publisher and the epoch every frame carries.
pub(super) struct Recompute<'a> {
    pub(super) publish: &'a dyn AgentStatusPublisher,
    pub(super) status_epoch: &'a StatusEpoch,
}

impl Recompute<'_> {
    fn publish_effective(
        &self,
        session_id: &SessionId,
        effective: &EffectiveEntry,
        active: bool,
        revision: i64,
        updated_at: i64,
    ) {
        if let Some(frame) = effective_frame(
            session_id,
            self.status_epoch,
            effective,
            active,
            revision,
            updated_at,
        ) {
            self.publish.publish(frame);
        }
    }

    fn log_occupant(
        &self,
        event: &str,
        session_id: &SessionId,
        occupant: &EffectiveEntry,
        revision: i64,
    ) {
        tracing::info!(
            event,
            session_id = %session_id,
            agent_id = occupant.process.agent_id.as_str(),
            status_epoch = self.status_epoch.as_str(),
            occupant_id = occupant.occupant_id.as_str(),
            source = occupant.source.as_str(),
            state = occupant.process.state.as_str(),
            revision,
            "agent-status occupant transition"
        );
    }

    /// v2 `recompute`.
    pub(super) fn run(
        &self,
        state: &mut RegistryState,
        session_id: &SessionId,
        now: i64,
        loss: CandidateLoss,
    ) {
        let RegistryState { entries, revision } = state;
        let Some(entry) = entries.get_mut(session_id) else {
            return;
        };
        if entry
            .integration
            .as_ref()
            .is_some_and(|integration| integration.lease_until <= now)
        {
            entry.integration = None;
        }
        let integration = entry.integration.as_ref();
        let candidate = integration
            .map(|integration| &integration.process)
            .or_else(|| entry.screen.as_ref().map(|screen| &screen.process))
            .cloned();
        // A screen-visible blocker prompt is direct evidence the agent is
        // waiting on a human, and it outranks a non-authoritative integration
        // that claims otherwise — most often one whose reporter went quiet
        // mid-turn.
        let blocker_overrides_integration = integration.is_some_and(|integration| {
            !full_lifecycle_agent(integration.process.agent_id)
                && integration.process.state != AgentRuntimeState::Blocked
                && entry.screen.as_ref().is_some_and(|screen| {
                    screen.visible_blocker
                        && screen.process.agent_id == integration.process.agent_id
                })
        });
        let has_integration = integration.is_some();
        let message = integration.and_then(|integration| integration.message.clone());
        let previous = entry.effective.clone();

        let Some(candidate) = candidate else {
            let Some(previous) = previous else { return };
            if !previous.occupant_live && loss != CandidateLoss::Withdrawn {
                return;
            }
            let revision = next_revision(revision, now);
            // An agent that finishes and then leaves is still done. Exit forces
            // the idle transition and keeps the row active so a completion
            // nobody has acknowledged outlives the process that earned it; only
            // an explicit withdrawal or session close retires the occupant.
            let retain = loss == CandidateLoss::Exit
                && (previous.process.state != AgentRuntimeState::Idle
                    || previous.completed_revision > 0);
            let mut exited = previous.clone();
            exited.process.state = AgentRuntimeState::Idle;
            exited.message = None;
            exited.occupant_live = false;
            exited.revision = revision;
            if previous.process.state != AgentRuntimeState::Idle {
                exited.completed_revision = revision;
            }
            exited.updated_at = now;
            let published = if retain { &exited } else { &previous };
            self.publish_effective(session_id, published, retain, revision, now);
            let event = if retain {
                "occupant_exited"
            } else {
                "occupant_inactive"
            };
            self.log_occupant(event, session_id, published, revision);
            entry.effective = retain.then(|| exited.clone());
            retire_process(entry, previous.process.process_key);
            return;
        };

        let source = if has_integration && !blocker_overrides_integration {
            AgentStatusSource::Integration
        } else {
            AgentStatusSource::Screen
        };
        let state = if blocker_overrides_integration {
            AgentRuntimeState::Blocked
        } else {
            candidate.state
        };
        let same_occupant = previous.as_ref().is_some_and(|previous| {
            previous.occupant_live && previous.process.process_key == candidate.process_key
        });
        if same_occupant
            && previous.as_ref().is_some_and(|previous| {
                previous.process.state == state
                    && previous.message == message
                    && previous.source == source
            })
        {
            return;
        }

        if let Some(previous) = previous.as_ref().filter(|_| !same_occupant) {
            let inactive_revision = next_revision(revision, now);
            entry.effective = None;
            self.publish_effective(session_id, previous, false, inactive_revision, now);
            self.log_occupant("occupant_inactive", session_id, previous, inactive_revision);
            retire_process(entry, previous.process.process_key);
        }

        let occupant_id = match previous.as_ref().filter(|_| same_occupant) {
            Some(previous) => previous.occupant_id.clone(),
            None => match mint_occupant_id() {
                Ok(occupant_id) => occupant_id,
                Err(error) => {
                    tracing::error!(%error, session = %session_id, "no occupant id could be minted; the status was not published");
                    return;
                }
            },
        };
        let revision = next_revision(revision, now);
        let completed_revision = match previous.as_ref().filter(|_| same_occupant) {
            Some(previous)
                if matches!(
                    previous.process.state,
                    AgentRuntimeState::Working | AgentRuntimeState::Blocked
                ) && state == AgentRuntimeState::Idle =>
            {
                revision
            }
            Some(previous) => previous.completed_revision,
            None => 0,
        };
        let mut process = candidate;
        process.state = state;
        let effective = EffectiveEntry {
            process,
            message,
            source,
            occupant_id,
            revision,
            completed_revision,
            updated_at: now,
            occupant_live: true,
        };
        entry.screen_absence_observed = false;
        self.publish_effective(
            session_id,
            &effective,
            true,
            effective.revision,
            effective.updated_at,
        );
        let event = if same_occupant {
            "occupant_updated"
        } else {
            "occupant_active"
        };
        self.log_occupant(event, session_id, &effective, effective.revision);
        entry.effective = Some(effective);
    }
}

fn mint_occupant_id() -> Result<AgentOccupantId, String> {
    let minted = mint_uuid().map_err(|error| error.to_string())?;
    AgentOccupantId::try_from(minted.as_str()).map_err(|error| error.to_string())
}
