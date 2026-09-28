//! Pane-local DOM reconciliation repair: a stalled catch-up arms a 3 s proof
//! deadline against the canonical watermark the DOM has not reached, repairs
//! locally at once, and escalates to recovering the owning generation only if
//! the deadline passes unreconciled. Both steps wait out a pointer gesture so
//! live DOM never moves under the reader's hand. Target-independent; the wasm
//! pane mount supplies the host and fires `on_deadline`. Ports
//! `apps/web/src/components/terminal/cell-terminal-dom-repair.ts`.

use roost_web_terminal::RendererEpochSeq;

/// How long the DOM may stay behind a captured watermark before escalation.
pub const DOM_RECONCILIATION_PROOF_MS: u64 = 3_000;

/// What the repair reads and does on the pane.
pub trait DomRepairHost {
    /// How far canonical has advanced, or `None` with no renderer.
    fn canonical(&self) -> Option<RendererEpochSeq>;
    /// How far the DOM has reconciled, or `None` with no renderer.
    fn reconciled(&self) -> Option<RendererEpochSeq>;
    /// In layout, foregrounded, and the page visible.
    fn actively_viewed(&self) -> bool;
    /// The view is accepted, active and baseline-ready.
    fn foreground_view_ready(&self) -> bool;
    /// A reader hold that a repair would rewrite under the reader.
    fn reader_hold_active(&self) -> bool;
    /// A pointer is down on the pane.
    fn pointer_gesture_active(&self) -> bool;
    /// The pane shows `catching_up`.
    fn catching_up(&self) -> bool;
    /// The local repair: drop predictions, release live holds, refresh the
    /// presentation and republish the view.
    fn repair_locally(&mut self);
    /// The escalation: recover the generation that owns the stalled view.
    fn recover_unreconciled(&mut self, watermark: &RendererEpochSeq);
}

/// One pane's repair target and deferrals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomRepair {
    deferred_stall: Option<RendererEpochSeq>,
    deferred_escalation: Option<RendererEpochSeq>,
    target: Option<RendererEpochSeq>,
    escalation_due_ms: Option<u64>,
}

fn still_unreconciled(host: &dyn DomRepairHost, watermark: &RendererEpochSeq) -> bool {
    let (Some(canonical), Some(reconciled)) = (host.canonical(), host.reconciled()) else {
        return false;
    };
    let (Some(epoch), Some(seq)) = (watermark.grid_epoch.as_ref(), watermark.seq) else {
        return false;
    };
    canonical.grid_epoch.as_ref() == Some(epoch)
        && canonical.seq.is_some_and(|canonical_seq| canonical_seq >= seq)
        && (reconciled.grid_epoch.as_ref() != Some(epoch)
            || reconciled.seq.is_none_or(|reconciled_seq| reconciled_seq < seq))
}

fn reached(host: &dyn DomRepairHost, watermark: &RendererEpochSeq) -> bool {
    host.reconciled().is_some_and(|reconciled| {
        reconciled.grid_epoch == watermark.grid_epoch
            && matches!((reconciled.seq, watermark.seq), (Some(have), Some(want)) if have >= want)
    })
}

/// One predicate for both gates: the stall gate and the proof deadline must
/// admit a repair on exactly the same facts.
fn warranted(host: &dyn DomRepairHost, watermark: &RendererEpochSeq) -> bool {
    host.actively_viewed()
        && host.foreground_view_ready()
        && !host.reader_hold_active()
        && still_unreconciled(host, watermark)
}

impl DomRepair {
    /// Nothing armed.
    pub fn new() -> Self {
        Self::default()
    }

    /// When the host must call `on_deadline`.
    pub fn next_deadline_ms(&self) -> Option<u64> {
        self.escalation_due_ms
    }

    /// Presentation reported a catch-up stall at `watermark`.
    pub fn on_catch_up_stalled(
        &mut self,
        watermark: RendererEpochSeq,
        now_ms: u64,
        host: &mut dyn DomRepairHost,
    ) {
        if self.target.is_some() || !warranted(host, &watermark) {
            return;
        }
        if host.pointer_gesture_active() {
            self.deferred_stall = Some(watermark);
            return;
        }
        self.arm_target(watermark, now_ms, host);
        if self.target.is_none() {
            return;
        }
        tracing::info!(target: "terminal", "dom reconcile stalled; repairing locally");
        host.repair_locally();
    }

    /// The renderer reconciled: retire a target the DOM has now reached.
    pub fn note_reconciled(&mut self, host: &dyn DomRepairHost) {
        if self.target.as_ref().is_some_and(|target| reached(host, target)) {
            self.clear_target();
        }
    }

    /// The pane's last pointer gesture settled: run what it deferred.
    pub fn resume_after_pointer_gesture(&mut self, now_ms: u64, host: &mut dyn DomRepairHost) {
        let stalled = self.deferred_stall.take();
        let had_stall = stalled.is_some();
        if let Some(stalled) = stalled {
            self.on_catch_up_stalled(stalled, now_ms, host);
        }
        let escalation = self.deferred_escalation.take();
        if !had_stall
            && host.catching_up()
            && !host.reader_hold_active()
            && let Some(current) = host.canonical()
        {
            self.on_catch_up_stalled(current, now_ms, host);
        }
        if let Some(escalation) = escalation {
            self.recover(escalation, host);
        }
    }

    /// The proof deadline passed.
    pub fn on_deadline(&mut self, now_ms: u64, host: &mut dyn DomRepairHost) {
        if !self.escalation_due_ms.is_some_and(|due| due <= now_ms) {
            return;
        }
        self.escalation_due_ms = None;
        if let Some(target) = self.target.clone() {
            self.recover(target, host);
        }
    }

    /// Forget the target and any deferred stall.
    pub fn clear_dom_stall_recovery(&mut self) {
        self.clear_target();
        self.deferred_stall = None;
    }

    fn arm_target(&mut self, watermark: RendererEpochSeq, now_ms: u64, host: &dyn DomRepairHost) {
        if !still_unreconciled(host, &watermark) {
            return;
        }
        let covered = self.target.as_ref().is_some_and(|held| {
            held.grid_epoch == watermark.grid_epoch
                && matches!((held.seq, watermark.seq), (Some(held), Some(next)) if held <= next)
        });
        if covered {
            return;
        }
        self.clear_target();
        self.target = Some(watermark);
        self.escalation_due_ms = Some(now_ms + DOM_RECONCILIATION_PROOF_MS);
    }

    fn recover(&mut self, watermark: RendererEpochSeq, host: &mut dyn DomRepairHost) {
        if self.target.as_ref() != Some(&watermark) {
            return;
        }
        // A decline leaves no armed target: the stall gate reads a present
        // target as "recovery owns this", which would retire the pane's only
        // repair for good.
        if !warranted(host, &watermark) {
            self.clear_target();
            return;
        }
        if host.pointer_gesture_active() {
            self.deferred_escalation = Some(watermark);
            return;
        }
        tracing::warn!(target: "terminal", epoch = ?watermark.grid_epoch, seq = ?watermark.seq,
            layer = "dom_reconcile", action = "redial", "cell.foreground_stall");
        host.recover_unreconciled(&watermark);
    }

    fn clear_target(&mut self) {
        self.escalation_due_ms = None;
        self.target = None;
        self.deferred_escalation = None;
    }
}
