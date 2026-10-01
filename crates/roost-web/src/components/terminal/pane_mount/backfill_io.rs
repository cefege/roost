//! The pane's scrollback pager I/O: history reads through the coordinator,
//! the frame yield between splices, and the retry sleeps, each answered back
//! into the pager. Every callback holds a `Weak`, so a dropped pane is a
//! no-op. Ports the RPC and timer halves of
//! `apps/web/src/renderer/scrollbackBackfill.ts` for one pane.

use roost_client_core::client::rpc::calls::terminal_pane::ScrollbackCells;
use roost_web_terminal::backfill::{BackfillAction, ScrollbackPage};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

use super::PaneShared;

/// Perform the pager's work: history reads, frame yields, retry sleeps.
pub(super) fn perform_backfill(shared: &PaneShared, work: Vec<BackfillAction>) {
    for action in work {
        match action {
            BackfillAction::Fetch { wave, request } => fetch_history(shared, wave, request),
            BackfillAction::AwaitAnimationFrame { wave } => {
                next_frame(shared, move |shared| {
                    let follow = {
                        let mut state = shared.state.borrow_mut();
                        let mut renderer = shared.renderer.borrow_mut();
                        state.backfill.on_animation_frame(wave, &mut *renderer)
                    };
                    perform_backfill(shared, follow);
                });
            }
            BackfillAction::ArmFetchRetry { wave, delay_ms } => {
                delay(shared, delay_ms, move |shared| {
                    let follow = {
                        let mut state = shared.state.borrow_mut();
                        let mut renderer = shared.renderer.borrow_mut();
                        state.backfill.on_fetch_retry_due(wave, &mut *renderer)
                    };
                    perform_backfill(shared, follow);
                });
            }
            BackfillAction::ArmDeferredRearm { timer, delay_ms } => {
                delay(shared, delay_ms, move |shared| {
                    let follow = {
                        let mut state = shared.state.borrow_mut();
                        let mut renderer = shared.renderer.borrow_mut();
                        state.backfill.on_deferred_rearm_due(timer, &mut *renderer)
                    };
                    perform_backfill(shared, follow);
                });
            }
            BackfillAction::FindSettled { row, painted } => {
                super::find_io::on_row_settled(shared, row, painted)
            }
        }
    }
}

fn delay(shared: &PaneShared, delay_ms: u64, then: impl FnOnce(&PaneShared) + 'static) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let weak = shared.weak_self();
    let callback = Closure::once_into_js(move || {
        if let Some(shared) = weak.upgrade().filter(|shared| !shared.disposed.get()) {
            then(&shared);
        }
    });
    let delay = i32::try_from(delay_ms).unwrap_or(i32::MAX);
    let _ = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), delay);
}

/// Run `then` on the next animation frame: a spliced page yields one frame
/// before the next splice so a long backfill never blocks paint.
fn next_frame(shared: &PaneShared, then: impl FnOnce(&PaneShared) + 'static) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let weak = shared.weak_self();
    let callback = Closure::once_into_js(move |_timestamp: f64| {
        if let Some(shared) = weak.upgrade().filter(|shared| !shared.disposed.get()) {
            then(&shared);
        }
    });
    let _ = window.request_animation_frame(callback.unchecked_ref());
}

fn fetch_history(
    shared: &PaneShared,
    wave: u64,
    request: roost_web_terminal::backfill::ScrollbackPageRequest,
) {
    shared.panes.note_backfill_request(&shared.session_id);
    let rpc = shared.pump.rpc();
    let weak = shared.weak_self();
    let call = ScrollbackCells {
        session_id: request.session_id,
        end_row: u64::from(request.end_row),
        max_rows: request.max_rows,
        grid_epoch: request.grid_epoch,
    };
    wasm_bindgen_futures::spawn_local(async move {
        let answer = rpc.call(&call).await;
        let Some(shared) = weak.upgrade().filter(|shared| !shared.disposed.get()) else {
            return;
        };
        let follow = {
            let mut state = shared.state.borrow_mut();
            let mut renderer = shared.renderer.borrow_mut();
            match answer {
                Ok(page) => state.backfill.on_page(
                    wave,
                    ScrollbackPage {
                        rows: page.rows,
                        start_row: page.start_row,
                        end_row: page.end_row,
                        cols: page.cols,
                        scrollback_total: page.scrollback_total,
                        grid_epoch: page.grid_epoch,
                        history_floor: page.history_floor,
                    },
                    &mut *renderer,
                ),
                Err(error) => {
                    tracing::warn!(target: "scrollback", session_id = %shared.session_id,
                        %error, "scrollback page read failed");
                    state.backfill.on_fetch_failed(wave, &mut *renderer)
                }
            }
        };
        perform_backfill(&shared, follow);
    });
}
