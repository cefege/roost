//! The pane's predictive-echo host: the `PredictiveEcho` state machine from
//! `roost-client-core`, the overlay it paints through, the animation-frame
//! batching of those paints and the timer that abandons a never-echoed guess.
//! The terminal pane calls `predict` per keystroke, `note_input_written` per
//! admission acknowledgement and `on_frame` per frame; `on_cursor` hands the
//! predicted caret to `CellGridRenderer::set_predicted_cursor`. Ports the host
//! half of v2's `apps/web/src/renderer/predictiveEcho.ts` (overlay, painter,
//! `setTimeout` expiry scheduler, `refreshPreference`, `clear`, `dispose`).

use std::cell::RefCell;
use std::rc::Rc;

use roost_client_core::client::predictive_echo::PredictiveEcho;
use roost_client_core::client::predictive_echo::report::EchoDebug;
use roost_client_core::store::prefs::PredictMode;
use roost_protocol::cell::CellGridFrame;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::Element;

use super::painter::{PaintFlush, PredictionPainter};
use super::{PredictiveEchoOverlay, plan_paint};
use crate::cell_renderer_dom::DomResult;

/// Where the painted caret goes: the renderer's `set_predicted_cursor`. It is
/// called inside the host's own flush, so it must not call back into the host.
type CursorSink = Box<dyn FnMut(Option<u32>)>;

struct EchoHostState {
    echo: PredictiveEcho,
    overlay: PredictiveEchoOverlay,
    painter: PredictionPainter,
    on_cursor: CursorSink,
    animation_frame: Option<i32>,
    expiry_timeout: Option<i32>,
    flush_callback: Option<js_sys::Function>,
    expiry_callback: Option<js_sys::Function>,
    disposed: bool,
}

/// One pane's predictive echo, wired to the DOM and the clock.
///
/// The two callbacks are created once and reused for every frame and timer,
/// and `Drop` cancels whatever is still scheduled: a callback firing after its
/// closure was dropped is a trap, not a no-op.
pub struct PredictiveEchoHost {
    state: Rc<RefCell<EchoHostState>>,
    _flush: Closure<dyn FnMut(f64)>,
    _expire: Closure<dyn FnMut()>,
}

impl std::fmt::Debug for PredictiveEchoHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PredictiveEchoHost").finish_non_exhaustive()
    }
}

impl PredictiveEchoHost {
    /// Attach an overlay to the pane's `.cell-viewport` (the renderer's
    /// `prediction_host`) and start an empty burst in `mode`.
    pub fn new(
        viewport: &Element,
        mode: PredictMode,
        on_cursor: impl FnMut(Option<u32>) + 'static,
    ) -> DomResult<Self> {
        let state = Rc::new(RefCell::new(EchoHostState {
            echo: PredictiveEcho::new(mode),
            overlay: PredictiveEchoOverlay::new(viewport)?,
            painter: PredictionPainter::default(),
            on_cursor: Box::new(on_cursor),
            animation_frame: None,
            expiry_timeout: None,
            flush_callback: None,
            expiry_callback: None,
            disposed: false,
        }));
        let weak = Rc::downgrade(&state);
        let flush = Closure::<dyn FnMut(f64)>::new(move |_timestamp: f64| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let Ok(mut state) = state.try_borrow_mut() else {
                return;
            };
            state.animation_frame = None;
            state.flush();
        });
        let weak = Rc::downgrade(&state);
        let expire = Closure::<dyn FnMut()>::new(move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let Ok(mut state) = state.try_borrow_mut() else {
                return;
            };
            state.expiry_timeout = None;
            if state.disposed {
                return;
            }
            state.echo.expire_predictions(now_ms());
            state.after_change();
        });
        if let Ok(mut owned) = state.try_borrow_mut() {
            owned.flush_callback = Some(flush.as_ref().unchecked_ref::<js_sys::Function>().clone());
            owned.expiry_callback =
                Some(expire.as_ref().unchecked_ref::<js_sys::Function>().clone());
        }
        tracing::debug!(target: "echo", mode = mode.as_str(), "predictive echo attached");
        Ok(Self {
            state,
            _flush: flush,
            _expire: expire,
        })
    }

    /// A keystroke's bytes, admitted as `input_seq`.
    pub fn predict(&self, bytes: &[u8], input_seq: u64) {
        self.change(|echo, now| echo.predict(bytes, input_seq, now));
    }

    /// The worker acknowledged writing every byte up to `input_seq`.
    pub fn note_input_written(&self, input_seq: u64) {
        self.change(|echo, now| echo.note_input_written(input_seq, now));
    }

    /// An authoritative frame landed. `scrollback_appended` is the batch's own
    /// history signal, which a coalesced frame no longer lists.
    pub fn on_frame(&self, frame: &CellGridFrame, scrollback_appended: bool) {
        self.change(|echo, now| echo.on_frame(frame, now, scrollback_appended));
    }

    /// Apply a Settings change immediately, even while the pane is idle.
    pub fn refresh_preference(&self, mode: PredictMode) {
        self.change(|echo, _| echo.set_mode(mode));
    }

    /// The pane's DOM-stall watchdog wipes the burst.
    pub fn clear(&self) {
        self.change(|echo, _| echo.clear());
    }

    /// The burst's internal state, for the smoke tier's reset accounting.
    pub fn debug(&self) -> Option<EchoDebug> {
        let mut state = self.state.try_borrow_mut().ok()?;
        Some(state.echo.debug())
    }

    /// Cancel the frame and the timer, detach the overlay, drop the burst.
    pub fn dispose(&self) {
        let Ok(mut state) = self.state.try_borrow_mut() else {
            return;
        };
        if state.disposed {
            return;
        }
        state.disposed = true;
        state.cancel_expiry();
        if state.painter.cancel()
            && let (Some(window), Some(id)) = (web_sys::window(), state.animation_frame.take())
        {
            let _ = window.cancel_animation_frame(id);
        }
        state.overlay.dispose();
        state.echo.dispose();
        tracing::debug!(target: "echo", "predictive echo disposed");
    }

    fn change(&self, apply: impl FnOnce(&mut PredictiveEcho, u64)) {
        let Ok(mut state) = self.state.try_borrow_mut() else {
            return;
        };
        if state.disposed {
            return;
        }
        apply(&mut state.echo, now_ms());
        state.after_change();
    }
}

impl Drop for PredictiveEchoHost {
    fn drop(&mut self) {
        self.dispose();
    }
}

impl EchoHostState {
    /// Re-arm the expiry timer from the machine's own delay, and hand the
    /// painter whatever the machine now wants shown.
    fn after_change(&mut self) {
        self.rearm_expiry();
        if self.echo.take_repaint() {
            let paint = self.echo.paint_request();
            if self.painter.request(paint) {
                self.schedule_flush();
            }
        }
    }

    fn schedule_flush(&mut self) {
        let scheduled = match (web_sys::window(), self.flush_callback.as_ref()) {
            (Some(window), Some(callback)) => window.request_animation_frame(callback).ok(),
            _ => None,
        };
        match scheduled {
            Some(id) => self.animation_frame = Some(id),
            // No animation frame to wait for: paint inline rather than never.
            None => self.flush(),
        }
    }

    fn flush(&mut self) {
        match self.painter.flush() {
            None => {}
            Some(PaintFlush::Clear) => {
                self.overlay.clear();
                (self.on_cursor)(None);
            }
            Some(PaintFlush::Paint(paint)) => {
                (self.on_cursor)(paint.caret_col);
                if self.overlay.paint(&plan_paint(&paint)).is_err() {
                    tracing::warn!(target: "echo", "the document refused a prediction cell");
                }
            }
        }
    }

    fn cancel_expiry(&mut self) {
        if let (Some(window), Some(id)) = (web_sys::window(), self.expiry_timeout.take()) {
            window.clear_timeout_with_handle(id);
        }
    }

    fn rearm_expiry(&mut self) {
        self.cancel_expiry();
        let (Some(delay), Some(window), Some(callback)) = (
            self.echo.expiry_delay_ms(),
            web_sys::window(),
            self.expiry_callback.as_ref(),
        ) else {
            return;
        };
        let delay = i32::try_from(delay).unwrap_or(i32::MAX);
        self.expiry_timeout = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(callback, delay)
            .ok();
    }
}

/// The page's monotonic clock in whole milliseconds, which is the unit every
/// predictor timing rule is written in.
fn now_ms() -> u64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or(0, |performance| performance.now().max(0.0) as u64)
}
