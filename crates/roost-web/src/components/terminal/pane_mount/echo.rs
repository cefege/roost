//! The pane's predictive echo: the `PredictiveEchoHost` painting guesses into
//! the renderer's live grid, fed each keystroke's admission, each settled
//! outcome from the core's per-view feed, and each painted frame; wiped when a
//! batch's fate is refused or uncertain and by the DOM-stall repair. Ports the
//! predictor wiring of `apps/web/src/components/terminal/cell-terminal-renderer.ts`
//! and `cell-terminal-input.ts`.

use std::rc::Rc;

use roost_client_core::store::prefs::PredictMode;
use roost_protocol::cell::CellGridFrame;
use roost_web_terminal::echo_overlay::PredictiveEchoHost;

use super::PaneShared;
use crate::components::terminal::pane_echo_feedback::{
    EchoFeedback, admission_echo_feedback, outcome_echo_feedback,
};

/// The mounted host and the preference it was last told.
pub(in crate::components::terminal) struct PaneEcho {
    host: PredictiveEchoHost,
    mode: PredictMode,
}

fn predict_mode(shared: &PaneShared) -> Option<PredictMode> {
    let core = shared.pump.core();
    let core = core.try_borrow().ok()?;
    Some(core.store().prefs.predict)
}

/// Attach the overlay to the renderer's live grid.
pub(super) fn attach(shared: &PaneShared) {
    let mode = predict_mode(shared).unwrap_or(PredictMode::Adaptive);
    let viewport = shared.renderer.borrow().prediction_host().clone();
    let renderer = Rc::downgrade(&shared.renderer);
    let host = PredictiveEchoHost::new(&viewport, mode, move |column| {
        if let Some(renderer) = renderer.upgrade()
            && let Ok(mut renderer) = renderer.try_borrow_mut()
        {
            renderer.set_predicted_cursor(column);
        }
    });
    match host {
        Ok(host) => *shared.echo.borrow_mut() = Some(PaneEcho { host, mode }),
        Err(error) => {
            tracing::warn!(target: "echo", session_id = %shared.session_id, ?error,
                "predictive echo failed to attach");
        }
    }
}

/// Dispose the host and forget the view's outcomes.
pub(super) fn detach(shared: &PaneShared) {
    if let Some(echo) = shared.echo.borrow_mut().take() {
        echo.host.dispose();
    }
    if let Ok(mut core) = shared.pump.core().try_borrow_mut() {
        core.store_mut().input.outcome_feed.forget_view(&shared.view_id);
    }
}

/// A batch was just dispatched from this view: answer its admission, then
/// anything that already settled (a batch with no route settles at once).
pub(super) fn after_dispatch(shared: &PaneShared, bytes: &[u8], predicts: bool) {
    let admission = {
        let core = shared.pump.core();
        let Ok(mut core) = core.try_borrow_mut() else {
            return;
        };
        core.store_mut().input.outcome_feed.take_admission(&shared.view_id)
    };
    if let Some(feedback) = admission.and_then(|answer| admission_echo_feedback(&answer, predicts)) {
        apply(shared, bytes, feedback);
    }
    drain_outcomes(shared);
}

/// Apply every outcome settled for this view since the last drain, and a
/// changed Settings preference.
pub(super) fn drain_outcomes(shared: &PaneShared) {
    let (outcomes, mode) = {
        let core = shared.pump.core();
        let Ok(mut core) = core.try_borrow_mut() else {
            return;
        };
        let mode = core.store().prefs.predict;
        (core.store_mut().input.outcome_feed.take_outcomes(&shared.view_id), mode)
    };
    if let Some(echo) = shared.echo.borrow_mut().as_mut()
        && echo.mode != mode
    {
        echo.mode = mode;
        echo.host.refresh_preference(mode);
    }
    for outcome in &outcomes {
        apply(shared, &[], outcome_echo_feedback(outcome));
    }
}

/// A frame was painted: reconcile every guess against it.
pub(super) fn on_frame(shared: &PaneShared, frame: &CellGridFrame, scrollback_appended: bool) {
    if let Some(echo) = shared.echo.borrow().as_ref() {
        echo.host.on_frame(frame, scrollback_appended);
    }
}

/// Wipe every guess and the predicted caret.
pub(super) fn clear(shared: &PaneShared) {
    if let Some(echo) = shared.echo.borrow().as_ref() {
        echo.host.clear();
    }
    if let Ok(mut renderer) = shared.renderer.try_borrow_mut() {
        renderer.set_predicted_cursor(None);
    }
}

fn apply(shared: &PaneShared, bytes: &[u8], feedback: EchoFeedback) {
    match feedback {
        EchoFeedback::Predict { input_seq } => {
            if let Some(echo) = shared.echo.borrow().as_ref() {
                echo.host.predict(bytes, input_seq);
            }
        }
        EchoFeedback::Written { input_seq } => {
            if let Some(echo) = shared.echo.borrow().as_ref() {
                echo.host.note_input_written(input_seq);
            }
        }
        EchoFeedback::Clear { status, reason } => {
            tracing::info!(target: "input", session_id = %shared.session_id, status, %reason,
                "input.drop_burst");
            clear(shared);
        }
    }
}
