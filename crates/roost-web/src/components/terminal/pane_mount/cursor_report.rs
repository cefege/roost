//! The pane's cursor report: on each 500ms poll deadline, a pane that may do
//! foreground work and whose painted cursor moved tells the coordinator where
//! it is (`SessionsCursorPos`), so other viewers draw its ghost cursor. The
//! decision is `CursorPollTicker`'s; the deadline rides the pane's one timer.
//! Ports the `registerCursorPoll` arm of
//! `apps/web/src/components/terminal/cell-terminal-renderer.ts`.

use roost_client_core::client::rpc::calls::terminal_pane::CursorPos;
use roost_web_terminal::scheduler::{CursorPollPane, CursorPollReading};

use super::PaneShared;

/// The pane's identity in its own poll ticker.
pub(super) const CURSOR_POLL_PANE: CursorPollPane = CursorPollPane::new(0);

/// The pane timer fired at `now_ms`: report the cursor when the poll is due
/// and it moved.
pub(super) fn on_deadline(shared: &PaneShared, now_ms: u64) {
    let due = {
        let mut state = shared.state.borrow_mut();
        if !state.cursor_poll.take_due(now_ms) {
            return;
        }
        let (cursor_col, cursor_row) = state.cursor.unwrap_or((0, 0));
        let reading = CursorPollReading {
            foreground_work_allowed: state.cursor.is_some()
                && state.flags.view_active()
                && state.page_visible,
            cursor_row,
            cursor_col,
        };
        state.cursor_poll.poll(CURSOR_POLL_PANE, reading)
    };
    let Some(position) = due else {
        return;
    };
    let call = CursorPos {
        session_id: shared.session_id.clone(),
        col: position.col,
        row: position.row,
    };
    let rpc = shared.pump.rpc();
    let session_id = shared.session_id.clone();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(error) = rpc.call(&call).await {
            tracing::debug!(target: "terminal", %session_id, %error, "cursor report failed");
        }
    });
}
