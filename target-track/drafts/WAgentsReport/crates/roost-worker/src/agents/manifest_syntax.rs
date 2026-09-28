//! The constructors the pinned manifests are written in: one call per rule and
//! per gate, so `agents::manifests` reads like the Herdr tables it adapts
//! rather than as nested struct literals. The object-literal shape of
//! `apps/worker/src/agents/manifests.ts`; used only by `agents::manifests`.

use roost_protocol::wire::agent_status::AgentRuntimeState;

use crate::agents::BuiltinAgentId;
use crate::agents::manifest_engine::{AgentManifest, ManifestGate, ManifestRegion, ManifestRule};

pub(super) const fn manifest(id: BuiltinAgentId, rules: &'static [ManifestRule]) -> AgentManifest {
    AgentManifest { id, rules }
}

const fn rule(
    id: &'static str,
    state: Option<AgentRuntimeState>,
    priority: u32,
    gate: ManifestGate,
) -> ManifestRule {
    ManifestRule {
        id,
        state,
        priority,
        region: ManifestRegion::WholeRecent,
        visible: false,
        skip_state_update: false,
        gate,
    }
}

pub(super) const fn blocked(id: &'static str, priority: u32, gate: ManifestGate) -> ManifestRule {
    rule(id, Some(AgentRuntimeState::Blocked), priority, gate)
}

pub(super) const fn working(id: &'static str, priority: u32, gate: ManifestGate) -> ManifestRule {
    rule(id, Some(AgentRuntimeState::Working), priority, gate)
}

pub(super) const fn idle(id: &'static str, priority: u32, gate: ManifestGate) -> ManifestRule {
    rule(id, Some(AgentRuntimeState::Idle), priority, gate)
}

pub(super) const fn unknown(id: &'static str, priority: u32, gate: ManifestGate) -> ManifestRule {
    rule(id, None, priority, gate)
}

impl ManifestRule {
    pub(super) const fn at(mut self, region: ManifestRegion) -> Self {
        self.region = region;
        self
    }

    pub(super) const fn visible(mut self) -> Self {
        self.visible = true;
        self
    }

    pub(super) const fn skipping_state_update(mut self) -> Self {
        self.skip_state_update = true;
        self
    }
}

pub(super) const fn contains(needles: &'static [&'static str]) -> ManifestGate {
    ManifestGate {
        contains: needles,
        ..ManifestGate::NONE
    }
}

pub(super) const fn regex(patterns: &'static [&'static str]) -> ManifestGate {
    ManifestGate {
        regex: patterns,
        ..ManifestGate::NONE
    }
}

pub(super) const fn line_regex(patterns: &'static [&'static str]) -> ManifestGate {
    ManifestGate {
        line_regex: patterns,
        ..ManifestGate::NONE
    }
}

pub(super) const fn any(gates: &'static [ManifestGate]) -> ManifestGate {
    ManifestGate {
        any: gates,
        ..ManifestGate::NONE
    }
}

pub(super) const fn all(gates: &'static [ManifestGate]) -> ManifestGate {
    ManifestGate {
        all: gates,
        ..ManifestGate::NONE
    }
}
