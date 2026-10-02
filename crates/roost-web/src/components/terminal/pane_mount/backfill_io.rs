//! The pane's scrollback pager I/O: history reads through the elected direct
//! carrier or the coordinator, the frame yield between splices, and the retry
//! sleeps, each answered back into the pager. Every callback holds a `Weak`, so
//! a dropped pane is a no-op. Ports the RPC and timer halves of
//! `apps/web/src/renderer/scrollbackBackfill.ts` for one pane, and the carrier
//! choice of `apps/web/src/lib/scrollbackDirectHistory.ts`.

use roost_client_core::client::rpc::calls::terminal_pane::{ScrollbackCells, ScrollbackCellsPage};

use crate::pump::DirectHistoryAnswer;
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

/// One history page for the pager: off the elected direct carrier when one owns
/// the session, else from the coordinator. A direct read the carrier could not
/// serve falls back to the coordinator once; one the worker refused, or one
/// whose route moved while it was in flight, fails the fetch (v2
/// `lib/scrollbackDirectHistory.ts` `requestScrollbackPage`).
fn fetch_history(
    shared: &PaneShared,
    wave: u64,
    request: roost_web_terminal::backfill::ScrollbackPageRequest,
) {
    shared.panes.note_backfill_request(&shared.session_id);
    let call = ScrollbackCells {
        session_id: request.session_id,
        end_row: u64::from(request.end_row),
        max_rows: request.max_rows,
        grid_epoch: request.grid_epoch,
    };
    let Some(route) = shared.pump.elected_direct_history_route(&call.session_id) else {
        fetch_from_coordinator(shared, wave, call);
        return;
    };
    let weak = shared.weak_self();
    let retry = call.clone();
    let sent_on = route.clone();
    shared
        .pump
        .read_direct_history(&route, &call, move |answer| {
            let Some(shared) = weak.upgrade().filter(|shared| !shared.disposed.get()) else {
                return;
            };
            match answer {
                DirectHistoryAnswer::Page(page) => {
                    shared
                        .panes
                        .note_direct_history_response(&shared.session_id);
                    settle_page(&shared, wave, Ok(page));
                }
                DirectHistoryAnswer::Overlimit => fetch_from_coordinator(&shared, wave, retry),
                DirectHistoryAnswer::Lost(reason) => {
                    let still_elected = shared
                        .pump
                        .elected_direct_history_route(&shared.session_id)
                        .is_some_and(|token| token == sent_on);
                    if still_elected {
                        tracing::info!(target: "scrollback", session_id = %shared.session_id,
                        %reason, "direct history read lost; reading from the coordinator");
                        fetch_from_coordinator(&shared, wave, retry);
                    } else {
                        settle_page(
                            &shared,
                            wave,
                            Err(format!(
                                "direct terminal scrollback route changed: {reason}"
                            )),
                        );
                    }
                }
                DirectHistoryAnswer::Refused(reason) => settle_page(
                    &shared,
                    wave,
                    Err(format!(
                        "direct terminal scrollback request was rejected: {reason}"
                    )),
                ),
            }
        });
}

fn fetch_from_coordinator(shared: &PaneShared, wave: u64, call: ScrollbackCells) {
    let rpc = shared.pump.rpc();
    let weak = shared.weak_self();
    wasm_bindgen_futures::spawn_local(async move {
        let answer = rpc.call(&call).await.map_err(|error| error.to_string());
        if let Some(shared) = weak.upgrade().filter(|shared| !shared.disposed.get()) {
            settle_page(&shared, wave, answer);
        }
    });
}

fn settle_page(shared: &PaneShared, wave: u64, answer: Result<ScrollbackCellsPage, String>) {
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
    perform_backfill(shared, follow);
}
