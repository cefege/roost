//! Cell-emission gate attribution: which gate withholds a channel's cell frames
//! (resize capture, baseline, synchronized output), since when, and how many
//! emits it swallowed. Split from `session::cell_scheduler` (v2
//! `apps/worker/src/session/session-cell-scheduler.ts` `noteCellGateSuppression`
//! and `session-emit.ts` `cellEmissionGates`); called by `session::emit`,
//! `session::cell_scheduler`, `session::sync_output` and `runtime::channel_delivery`.

use roost_protocol::wire::brand::ChannelId;

use super::emit::CellEmitter;

/// v2 `CELL_GATE_BUDGET_MS`: past the keeper's own per-command budget a
/// resize/baseline gate is corruption, not latency.
pub const CELL_GATE_BUDGET_MS: i64 = 2_500;

/// Which gate is withholding cell frames for a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellGate {
    /// A sequenced live resize has not resolved its boundary.
    ResizeCapture,
    /// An active sink does not hold a complete baseline yet.
    Baseline,
    /// The application has an open synchronized-output (DEC 2026) frame.
    SyncOutput,
}

impl CellGate {
    pub fn as_str(self) -> &'static str {
        match self {
            CellGate::ResizeCapture => "resize_capture",
            CellGate::Baseline => "baseline",
            CellGate::SyncOutput => "sync_output",
        }
    }
}

/// v2 `CellGateSuppression`: which gate is withholding a channel, since when
/// (wall epoch ms, the clock every `now_ms` carries), and how many emits it ate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateSuppression {
    pub gate: CellGate,
    pub since_ms: i64,
    pub suppressed: u64,
    /// The gate outlived its ceiling (resize/baseline past the keeper budget, or
    /// a synchronized-output hold that tripped).
    pub over_budget: bool,
}

impl CellEmitter {
    /// Hold a channel's emission gate (v2 `cellEmissionGates.add`) and attribute it.
    pub fn hold_frames(&mut self, channel_id: ChannelId, gate: CellGate, now_ms: i64) {
        if self.emission_gates.insert(channel_id) {
            tracing::info!(%channel_id, gate = gate.as_str(), "a cell-emission gate was set");
        }
        self.attribute_gate(channel_id, gate, now_ms);
    }

    /// Release a hold. Whether an emit is owed is [`CellEmitter::is_dirty`]'s answer.
    pub fn release_frames(&mut self, channel_id: ChannelId) {
        if !self.emission_gates.remove(&channel_id) {
            return;
        }
        let held = self.gates.remove(&channel_id);
        tracing::info!(
            %channel_id,
            gate = held.map_or("none", |held| held.gate.as_str()),
            suppressed = held.map_or(0, |held| held.suppressed),
            "a cell-emission gate was released"
        );
    }

    /// What is withholding a channel, and for how long it has been.
    pub fn gate_suppression(&self, channel_id: ChannelId) -> Option<GateSuppression> {
        self.gates.get(&channel_id).copied()
    }

    /// Whether the emission gate holds this channel.
    pub fn gate_held(&self, channel_id: ChannelId) -> bool {
        self.emission_gates.contains(&channel_id)
    }

    fn attribute_gate(&mut self, channel_id: ChannelId, gate: CellGate, now_ms: i64) {
        let held = self.gates.entry(channel_id).or_insert(GateSuppression {
            gate,
            since_ms: now_ms,
            suppressed: 0,
            over_budget: false,
        });
        if held.gate != gate {
            *held = GateSuppression {
                gate,
                since_ms: now_ms,
                suppressed: 0,
                over_budget: false,
            };
        }
    }

    /// v2 `noteCellGateSuppression`: count one swallowed emit, and flag a
    /// resize/baseline gate past [`CELL_GATE_BUDGET_MS`] once.
    pub(crate) fn note_gate_suppression(
        &mut self,
        channel_id: ChannelId,
        gate: CellGate,
        now_ms: i64,
    ) {
        self.attribute_gate(channel_id, gate, now_ms);
        let Some(held) = self.gates.get_mut(&channel_id) else {
            return;
        };
        held.suppressed = held.suppressed.saturating_add(1);
        let age_ms = now_ms.saturating_sub(held.since_ms);
        if gate == CellGate::SyncOutput || held.over_budget || age_ms <= CELL_GATE_BUDGET_MS {
            return;
        }
        held.over_budget = true;
        tracing::warn!(%channel_id, gate = gate.as_str(), age_ms, budget_ms = CELL_GATE_BUDGET_MS, "terminal.gate_over_budget");
    }
}
