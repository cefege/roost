//! The bottom inset the notification dock must clear, as a CSS length
//! expression.
//!
//! Lives apart from the dock so the three bottom-chrome cases are decidable —
//! and testable — without a DOM. Ports
//! `apps/web/src/lib/notificationDockLift.ts`, whose separation from
//! `NotificationDock.tsx` exists for exactly this reason.
//!
//! The composer is the only bottom chrome whose height MOVES, so it is the only
//! case that reads a measurement; the other two are constants, and a constant is
//! the right answer for a chrome that does not change while a toast is on screen.

use crate::components::layout::shell_style::ComposerGeometry;

/// `--roost-notify-dock-lift` for the given bottom chrome.
#[must_use]
pub fn notification_dock_lift(composer: ComposerGeometry, compact: bool) -> String {
    // The viewport composer's own offset already folds the safe area and the
    // soft-keyboard inset, so the lift adds only its measured height.
    if composer.active {
        return format!(
            "calc(var(--term-chat-dock-offset) + {}px + var(--md-space-2))",
            composer.height_px
        );
    }
    if compact {
        return "var(--term-chat-dock-offset)".to_owned();
    }
    // Pointer layouts: clear the status bar and the in-pane composer's resting
    // row with one constant, so the dock never depends on route or pane count.
    "calc(var(--workbench-statusbar-height) + var(--term-chat-rest-height) + max(var(--kb-offset), 0px) + var(--md-space-4))".to_owned()
}
