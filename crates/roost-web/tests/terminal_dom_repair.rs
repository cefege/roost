//! A pane whose DOM stays behind canonical repairs locally at once and
//! escalates only after the 3 s proof deadline; a reader hold or a pointer
//! gesture defers both. Ports the DOM-recovery cases of
//! `apps/web/tests/cellTerminalPresentation.test.ts`.

use roost_web::components::terminal::dom_repair::{
    DOM_RECONCILIATION_PROOF_MS, DomRepair, DomRepairHost,
};
use roost_web_terminal::RendererEpochSeq;

fn mark(seq: u64) -> RendererEpochSeq {
    RendererEpochSeq {
        grid_epoch: Some("epoch-a".to_owned()),
        seq: Some(seq),
    }
}

struct Pane {
    canonical: RendererEpochSeq,
    reconciled: RendererEpochSeq,
    reader_hold: bool,
    pointer_down: bool,
    local_repairs: u32,
    recoveries: Vec<RendererEpochSeq>,
}

impl Pane {
    fn stalled() -> Self {
        Self {
            canonical: mark(2),
            reconciled: mark(1),
            reader_hold: false,
            pointer_down: false,
            local_repairs: 0,
            recoveries: Vec::new(),
        }
    }
}

impl DomRepairHost for Pane {
    fn canonical(&self) -> Option<RendererEpochSeq> {
        Some(self.canonical.clone())
    }
    fn reconciled(&self) -> Option<RendererEpochSeq> {
        Some(self.reconciled.clone())
    }
    fn actively_viewed(&self) -> bool {
        true
    }
    fn foreground_view_ready(&self) -> bool {
        true
    }
    fn reader_hold_active(&self) -> bool {
        self.reader_hold
    }
    fn pointer_gesture_active(&self) -> bool {
        self.pointer_down
    }
    fn catching_up(&self) -> bool {
        true
    }
    fn repair_locally(&mut self) {
        self.local_repairs += 1;
    }
    fn recover_unreconciled(&mut self, watermark: &RendererEpochSeq) {
        self.recoveries.push(watermark.clone());
    }
}

#[test]
fn escalation_waits_for_the_whole_three_second_proof_deadline() {
    let (mut pane, mut repair) = (Pane::stalled(), DomRepair::new());
    repair.on_catch_up_stalled(mark(2), 0, &mut pane);
    assert_eq!(pane.local_repairs, 1);
    repair.on_deadline(DOM_RECONCILIATION_PROOF_MS - 1, &mut pane);
    assert!(pane.recoveries.is_empty());
    repair.on_deadline(DOM_RECONCILIATION_PROOF_MS, &mut pane);
    assert_eq!(pane.recoveries, [mark(2)]);
}

#[test]
fn the_target_clears_only_once_the_dom_reaches_its_captured_watermark() {
    let (mut pane, mut repair) = (Pane::stalled(), DomRepair::new());
    repair.on_catch_up_stalled(mark(2), 0, &mut pane);
    repair.note_reconciled(&pane);
    assert_eq!(repair.next_deadline_ms(), Some(DOM_RECONCILIATION_PROOF_MS));
    pane.reconciled = mark(2);
    repair.note_reconciled(&pane);
    assert_eq!(
        repair.next_deadline_ms(),
        None,
        "reaching the watermark retires the target and its deadline"
    );
    repair.on_deadline(DOM_RECONCILIATION_PROOF_MS, &mut pane);
    assert!(pane.recoveries.is_empty());
}

#[test]
fn a_reader_hold_and_a_pointer_gesture_both_defer_the_repair() {
    let (mut held, mut held_repair) = (Pane::stalled(), DomRepair::new());
    held.reader_hold = true;
    held_repair.on_catch_up_stalled(mark(2), 0, &mut held);
    held_repair.on_deadline(DOM_RECONCILIATION_PROOF_MS, &mut held);
    assert_eq!((held.local_repairs, held.recoveries.len()), (0, 0));

    let (mut pointer, mut repair) = (Pane::stalled(), DomRepair::new());
    repair.on_catch_up_stalled(mark(2), 0, &mut pointer);
    assert_eq!(pointer.local_repairs, 1);
    pointer.pointer_down = true;
    repair.on_deadline(DOM_RECONCILIATION_PROOF_MS, &mut pointer);
    assert!(pointer.recoveries.is_empty());
    pointer.pointer_down = false;
    repair.resume_after_pointer_gesture(DOM_RECONCILIATION_PROOF_MS, &mut pointer);
    assert_eq!(pointer.recoveries, [mark(2)]);
}

#[test]
fn a_hold_that_declined_the_deadline_lets_the_next_stall_re_arm() {
    let (mut pane, mut repair) = (Pane::stalled(), DomRepair::new());
    repair.on_catch_up_stalled(mark(2), 0, &mut pane);
    pane.reader_hold = true;
    repair.on_deadline(DOM_RECONCILIATION_PROOF_MS, &mut pane);
    assert!(pane.recoveries.is_empty());

    pane.reader_hold = false;
    let start = DOM_RECONCILIATION_PROOF_MS;
    repair.on_catch_up_stalled(mark(2), start, &mut pane);
    repair.on_deadline(start + DOM_RECONCILIATION_PROOF_MS - 1, &mut pane);
    assert!(pane.recoveries.is_empty());
    repair.on_deadline(start + DOM_RECONCILIATION_PROOF_MS, &mut pane);
    assert_eq!(pane.recoveries.len(), 1);
}

#[test]
fn a_stall_during_a_pointer_gesture_repairs_when_the_pointer_lifts() {
    let (mut pane, mut repair) = (Pane::stalled(), DomRepair::new());
    pane.pointer_down = true;
    repair.on_catch_up_stalled(mark(2), 0, &mut pane);
    assert_eq!(pane.local_repairs, 0);
    pane.pointer_down = false;
    repair.resume_after_pointer_gesture(10, &mut pane);
    assert_eq!(pane.local_repairs, 1);
}
