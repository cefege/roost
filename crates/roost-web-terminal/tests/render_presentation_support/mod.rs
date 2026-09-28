//! Shared fixtures for the terminal presentation tests: a renderer stub that
//! only reports watermarks, the view statuses v2's tests use, and a pane
//! harness whose `advance` fires due deadlines at their due instant the way
//! fake timers do. Mirrors the fixtures in
//! `apps/web/tests/renderer/terminalPresentation.test.ts`.

#![allow(dead_code)]

use roost_web_terminal::presentation::RendererEpochSeq;
use roost_web_terminal::reader_intent::ReaderIntentReason;
use roost_web_terminal::terminal_presentation::{
    PresentationFrameMark, PresentationInputs, PresentationPane, PresentationRendererView,
    TerminalPresentationController, TerminalPresentationState, TerminalViewHandleStatus,
};

/// The view is accepted, active and has its baseline.
pub const ACCEPTED: TerminalViewHandleStatus = TerminalViewHandleStatus::Accepted {
    active: true,
    baseline_ready: true,
};

/// The production freeze shape: accepted and active, but the baseline never
/// arrived, so there is nothing live to paint.
pub const ACCEPTED_WITHOUT_BASELINE: TerminalViewHandleStatus =
    TerminalViewHandleStatus::Accepted {
        active: true,
        baseline_ready: false,
    };

pub fn watermark(grid_epoch: &str, seq: u64) -> RendererEpochSeq {
    RendererEpochSeq {
        grid_epoch: Some(grid_epoch.to_owned()),
        seq: Some(seq),
    }
}

/// Renderer stub for the paths that only read watermarks.
#[derive(Debug, Clone)]
pub struct StubRenderer {
    pub canonical: RendererEpochSeq,
    pub reconciled: RendererEpochSeq,
    pub reader_reason: Option<ReaderIntentReason>,
    pub cursor_blink: Option<bool>,
}

impl StubRenderer {
    pub fn at(canonical: RendererEpochSeq, reconciled: RendererEpochSeq) -> Self {
        Self {
            canonical,
            reconciled,
            reader_reason: None,
            cursor_blink: None,
        }
    }

    /// Canonical and reconciled are always equal, so a ready view never lands
    /// in `catching_up`.
    pub fn reconciled() -> Self {
        Self::at(watermark("epoch-a", 5), watermark("epoch-a", 5))
    }
}

impl PresentationRendererView for StubRenderer {
    fn canonical_epoch_seq(&self) -> RendererEpochSeq {
        self.canonical.clone()
    }

    fn reconciled_epoch_seq(&self) -> RendererEpochSeq {
        self.reconciled.clone()
    }

    fn reader_reason(&self) -> Option<ReaderIntentReason> {
        self.reader_reason
    }

    fn set_cursor_blink_enabled(&mut self, enabled: bool) {
        self.cursor_blink = Some(enabled);
    }
}

/// One pane driving one controller on a fake clock.
#[derive(Debug)]
pub struct PaneHarness {
    pub controller: TerminalPresentationController,
    pub now_ms: u64,
    pub pane: PresentationPane,
    pub status: Option<TerminalViewHandleStatus>,
    pub renderer: Option<StubRenderer>,
    pub stalled: Vec<RendererEpochSeq>,
}

impl PaneHarness {
    /// Active, focused, visible, with `status` and `renderer`.
    pub fn new(status: TerminalViewHandleStatus, renderer: StubRenderer) -> Self {
        Self {
            controller: TerminalPresentationController::new(),
            now_ms: 1_000_000,
            pane: PresentationPane {
                active: true,
                focused: true,
                page_visible: true,
            },
            status: Some(status),
            renderer: Some(renderer),
            stalled: Vec::new(),
        }
    }

    pub fn renderer_mut(&mut self) -> &mut StubRenderer {
        self.renderer
            .as_mut()
            .expect("the harness renderer is mounted")
    }

    pub fn state(&self) -> TerminalPresentationState {
        self.controller.state()
    }

    pub fn refresh(&mut self) -> TerminalPresentationState {
        let inputs = PresentationInputs {
            now_ms: self.now_ms,
            pane: self.pane,
            status: self.status,
            renderer: self.renderer.as_ref(),
        };
        self.controller.refresh_terminal_presentation(&inputs)
    }

    pub fn note_frame(
        &mut self,
        full: bool,
        grid_epoch: &str,
        seq: u64,
    ) -> TerminalPresentationState {
        let inputs = PresentationInputs {
            now_ms: self.now_ms,
            pane: self.pane,
            status: self.status,
            renderer: self.renderer.as_ref(),
        };
        let frame = PresentationFrameMark {
            full,
            grid_epoch,
            seq,
        };
        self.controller.note_frame_activity(frame, &inputs)
    }

    /// Move the clock forward, firing every deadline due inside the step at
    /// its own due instant, exactly as `vi.advanceTimersByTime` does.
    pub fn advance(&mut self, step_ms: u64) {
        let target_ms = self.now_ms + step_ms;
        for _ in 0..10_000 {
            let due_ms = match self.controller.next_deadline_ms() {
                Some(due_ms) if due_ms <= target_ms => due_ms,
                _ => {
                    self.now_ms = target_ms;
                    return;
                }
            };
            assert!(due_ms >= self.now_ms, "a deadline was armed in the past");
            self.now_ms = due_ms;
            let inputs = PresentationInputs {
                now_ms: self.now_ms,
                pane: self.pane,
                status: self.status,
                renderer: self.renderer.as_ref(),
            };
            if let Some(stalled) = self.controller.fire_due_deadline(&inputs) {
                self.stalled.push(stalled.0);
            }
        }
        panic!("the controller re-armed a due deadline without end");
    }
}
