//! What one pane knows about its view lease, read off the store, and the
//! startup notice that turns it into the card the operator sees while the
//! pane opens. Target-independent; read by `cell_terminal` and the wasm pane
//! mount. Ports `terminalViewportLoadingNotice` of
//! `apps/web/src/components/terminal/TerminalStartupOverlay.tsx` and the
//! loading gate of `apps/web/src/components/terminal/cell-terminal-presentation.ts`.

use roost_client_core::Store;
use roost_client_core::terminal::view::ViewIntent;
use roost_web_terminal::startup_progress::TerminalStartupStage;
use roost_web_terminal::terminal_presentation::TerminalViewHandleStatus;

/// One pane's view lease as the store holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneViewStatus {
    /// The lease state presentation reads.
    pub status: TerminalViewHandleStatus,
    /// The geometry the replica is fenced to.
    pub effective_cols: u32,
    /// The geometry the replica is fenced to.
    pub effective_rows: u32,
}

impl PaneViewStatus {
    /// Accepted, active and baseline-ready: a live painted stream is behind it.
    pub fn foreground_ready(&self) -> bool {
        self.status.accepted_with_baseline()
    }
}

/// The view's lease, or `None` before the pane opened one. A view the
/// authority has acknowledged is accepted; one still awaiting its answer is
/// pending.
pub fn view_handle_status(
    store: &Store,
    session_id: &str,
    view_id: &str,
) -> Option<PaneViewStatus> {
    let replica = store.terminal.get(session_id)?;
    let view = replica.view(view_id)?;
    let (effective_cols, effective_rows) = replica.effective_geometry();
    let status = if view.counted {
        TerminalViewHandleStatus::Accepted {
            active: matches!(view.intent, ViewIntent::Publish { .. }),
            baseline_ready: replica.baseline_ready(),
        }
    } else {
        TerminalViewHandleStatus::Pending
    };
    Some(PaneViewStatus {
        status,
        effective_cols,
        effective_rows,
    })
}

/// The card shown while a pane opens.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalStartupNotice {
    /// Which step the pane is on.
    pub stage: TerminalStartupStage,
    /// The announcement line.
    pub title: String,
    /// The technical detail, collapsed until a step is slow.
    pub detail: String,
    /// Chunked-baseline progress, `(received, total)`.
    pub progress: Option<(u32, u32)>,
    /// Why a stalled attach is stuck.
    pub stuck_reason: Option<String>,
    /// The pane's session, for smoke diagnostics.
    pub session_id: Option<String>,
}

/// The `data-stage` spelling of a startup stage.
pub const fn stage_attribute(stage: TerminalStartupStage) -> &'static str {
    match stage {
        TerminalStartupStage::Spawn => "spawn",
        TerminalStartupStage::Measure => "measure",
        TerminalStartupStage::Viewport => "viewport",
        TerminalStartupStage::Frame => "frame",
        TerminalStartupStage::Render => "render",
        TerminalStartupStage::Retry => "retry",
    }
}

fn notice(stage: TerminalStartupStage, title: &str, detail: String) -> TerminalStartupNotice {
    TerminalStartupNotice {
        stage,
        title: title.to_owned(),
        detail,
        progress: None,
        stuck_reason: None,
        session_id: None,
    }
}

/// The step a pane is on, from its spawn state and its view lease.
pub fn terminal_viewport_loading_notice(
    pending: bool,
    status: Option<PaneViewStatus>,
) -> TerminalStartupNotice {
    if pending {
        return notice(
            TerminalStartupStage::Spawn,
            "Starting terminal process",
            "Waiting for the coordinator to confirm the new PTY.".to_owned(),
        );
    }
    let Some(status) = status else {
        return notice(
            TerminalStartupStage::Measure,
            "Measuring terminal view",
            "Waiting for the visible pane size before requesting a screen.".to_owned(),
        );
    };
    match status.status {
        TerminalViewHandleStatus::Pending => notice(
            TerminalStartupStage::Viewport,
            "Requesting terminal viewport",
            "Waiting for the coordinator to accept this terminal view.".to_owned(),
        ),
        TerminalViewHandleStatus::Accepted {
            baseline_ready: true,
            ..
        } => notice(
            TerminalStartupStage::Render,
            "Rendering terminal screen",
            "The full screen arrived; waiting for browser layout and paint.".to_owned(),
        ),
        TerminalViewHandleStatus::Accepted { .. } => notice(
            TerminalStartupStage::Frame,
            "Waiting for terminal screen",
            format!(
                "View accepted at {}×{}; waiting for its full baseline.",
                status.effective_cols, status.effective_rows
            ),
        ),
        TerminalViewHandleStatus::Unavailable => notice(
            TerminalStartupStage::Retry,
            "Terminal stream unavailable",
            "The active view will retry on its next lease refresh.".to_owned(),
        ),
        TerminalViewHandleStatus::Rejected => notice(
            TerminalStartupStage::Retry,
            "Terminal view rejected",
            "Another terminal view changed this screen. Reconnecting automatically.".to_owned(),
        ),
    }
}

/// The facts that decide whether the startup card shows at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadingGate {
    /// In layout, visible and active.
    pub view_active: bool,
    /// The document is visible.
    pub page_visible: bool,
    /// The offline notice owns the pane.
    pub offline: bool,
    /// The renderer has reconciled at least one frame.
    pub has_reconciled_frame: bool,
    /// The spawn is still optimistic.
    pub pending: bool,
}

/// The card to show, or `None`. A pane that already painted keeps its frame
/// through a lease refresh rather than flashing the card over it.
pub fn loading_notice(
    gate: LoadingGate,
    status: Option<PaneViewStatus>,
) -> Option<TerminalStartupNotice> {
    let ready = status.is_some_and(|status| status.foreground_ready());
    let refreshing = gate.has_reconciled_frame
        && status.is_some_and(|status| match status.status {
            TerminalViewHandleStatus::Pending => true,
            TerminalViewHandleStatus::Accepted { baseline_ready, .. } => !baseline_ready,
            TerminalViewHandleStatus::Unavailable | TerminalViewHandleStatus::Rejected => false,
        });
    if !gate.view_active
        || !gate.page_visible
        || gate.offline
        || (gate.has_reconciled_frame && ready)
        || refreshing
    {
        return None;
    }
    Some(terminal_viewport_loading_notice(gate.pending, status))
}
