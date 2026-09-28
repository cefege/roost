//! The pure decision behind a pane's status dot: which of idle / receiving /
//! catching_up / detached the operator is shown for one pair of canonical and
//! reconciled watermarks, plus the view-status facts that decision reads.
//! `TerminalPresentationController` feeds it; nothing here owns a clock.
//! Ports the presentation half of `apps/web/src/store/terminal-stream-types.ts`
//! (the watermark shape reuses `RendererEpochSeq`).

use crate::presentation::RendererEpochSeq;

/// How long a delta keeps the pane reading `receiving`.
pub const FRAME_ACTIVITY_WINDOW_MS: u64 = 500;

/// How long an actively-viewed pane may sit without an accepted, baseline-ready
/// view before the absence becomes operator-visible. An ordinary attach or tab
/// switch resolves well inside it, so `detached` never flashes on a healthy pane.
pub const DETACHED_GRACE_MS: u64 = 1_000;

// The offline watch arms its re-claim on the `detached` edge; freshness must
// already have expired there or it would mask the edge that arms it.
const _: () = assert!(FRAME_ACTIVITY_WINDOW_MS < DETACHED_GRACE_MS);

/// What a pane's stream indicator shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerminalPresentationState {
    /// Quiet and healthy — and nothing else. No indicator.
    #[default]
    Idle,
    /// A delta landed inside the activity window and the DOM shows it.
    Receiving,
    /// Canonical is ahead of what the DOM reconciled.
    CatchingUp,
    /// The operator is looking at a pane with no live stream behind it.
    Detached,
}

impl TerminalPresentationState {
    /// The `data-state` token the indicator's stylesheet selects on.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Receiving => "receiving",
            Self::CatchingUp => "catching_up",
            Self::Detached => "detached",
        }
    }
}

/// The coordinator's answer to a pane's view declaration, reduced to the facts
/// presentation reads. Mirrors `TerminalViewHandleStatus` in
/// `apps/web/src/store/terminal-stream-types.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalViewHandleStatus {
    /// Declared, not yet answered.
    Pending,
    /// Accepted; `baseline_ready` once a complete full for the stream landed.
    Accepted { active: bool, baseline_ready: bool },
    /// The terminal's worker is gone.
    Unavailable,
    /// The declaration was refused.
    Rejected,
}

impl TerminalViewHandleStatus {
    /// Whether there is a live, painted stream behind the view.
    pub fn accepted_with_baseline(self) -> bool {
        matches!(
            self,
            Self::Accepted {
                active: true,
                baseline_ready: true
            }
        )
    }
}

/// The most recent delta a pane painted, and when it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPresentationActivity {
    pub grid_epoch: String,
    pub seq: u64,
    pub started_at_ms: u64,
}

/// Everything one presentation decision reads.
#[derive(Debug, Clone, Copy)]
pub struct TerminalPresentationInput<'a> {
    pub active: bool,
    pub accepted_with_baseline: bool,
    pub canonical: &'a RendererEpochSeq,
    pub reconciled: &'a RendererEpochSeq,
    pub activity: Option<&'a TerminalPresentationActivity>,
    pub now_ms: u64,
    /// When an actively-viewed pane entered the state of having no accepted,
    /// active, baseline-ready view; `None` when it is not in that state.
    pub not_ready_since_ms: Option<u64>,
}

/// Decide what the pane's indicator shows. Elapsed-time comparisons are written
/// as deadlines so a clock that steps backwards reads as "not yet elapsed"
/// exactly as a negative difference does.
pub fn derive_terminal_presentation_state(
    input: TerminalPresentationInput<'_>,
) -> TerminalPresentationState {
    // Absence of an indicator must mean exactly one thing — quiet and healthy.
    // A pane the operator is looking at with no live stream is a failure to
    // show, not silence to hide.
    if !input.active || !input.accepted_with_baseline {
        let grace_expired = input
            .not_ready_since_ms
            .is_some_and(|since_ms| input.now_ms >= since_ms.saturating_add(DETACHED_GRACE_MS));
        return if input.active && grace_expired {
            TerminalPresentationState::Detached
        } else {
            TerminalPresentationState::Idle
        };
    }
    if input.canonical != input.reconciled {
        return TerminalPresentationState::CatchingUp;
    }
    let fresh = input.activity.is_some_and(|activity| {
        input.canonical.grid_epoch.as_deref() == Some(activity.grid_epoch.as_str())
            && input.canonical.seq == Some(activity.seq)
            && input.now_ms < activity.started_at_ms.saturating_add(FRAME_ACTIVITY_WINDOW_MS)
    });
    if fresh {
        TerminalPresentationState::Receiving
    } else {
        TerminalPresentationState::Idle
    }
}
