//! Sending queued browser diagnostics after coordinator authentication is ready.
//!
//! A single in-flight call preserves queue order; failed batches return to the
//! front so transient transport loss does not silently discard the evidence,
//! and each consecutive failure doubles the wait before the next send.

use std::cell::Cell;
use std::rc::Rc;

use roost_client_core::client::rpc::calls::diag_debug_log_batch::DiagDebugLogBatch;
use tracing::Instrument as _;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

use super::DiagUploadQueue;
use super::layer::SEND_SPAN_TARGET;
use crate::pump::Pump;

const FLUSH_INTERVAL_MS: i32 = 1_000;
const BATCH_SIZE: usize = 64;
/// The longest a failing upload waits between sends. A signed-out tab or an
/// unreachable coordinator then costs one request a minute, not one a second.
const MAX_BACKOFF_MS: f64 = 60_000.0;

/// Whether a send is in flight, and when the next one may go after failures.
#[derive(Default)]
struct SendState {
    in_flight: Cell<bool>,
    failures: Cell<u32>,
    next_send_ms: Cell<f64>,
}

impl SendState {
    fn failed(&self, now_ms: f64) {
        let failures = self.failures.get().saturating_add(1);
        self.failures.set(failures);
        let exponent = i32::try_from(failures.min(16)).unwrap_or(16);
        let backoff = (f64::from(FLUSH_INTERVAL_MS) * 2_f64.powi(exponent)).min(MAX_BACKOFF_MS);
        self.next_send_ms.set(now_ms + backoff);
    }

    fn succeeded(&self) {
        self.failures.set(0);
        self.next_send_ms.set(0.0);
    }
}

pub(crate) fn install(pump: &Pump, queue: DiagUploadQueue) {
    let Some(window) = web_sys::window() else {
        tracing::error!(target: "diag_upload", "no window: diagnostic uploads not installed");
        return;
    };
    let state = Rc::new(SendState::default());
    let interval = {
        let pump = pump.clone();
        let queue = queue.clone();
        let state = state.clone();
        Closure::<dyn FnMut()>::new(move || flush(&pump, &queue, &state))
    };
    if window
        .set_interval_with_callback_and_timeout_and_arguments_0(
            interval.as_ref().unchecked_ref(),
            FLUSH_INTERVAL_MS,
        )
        .is_err()
    {
        tracing::error!(target: "diag_upload", "the browser refused the diagnostic upload interval");
    }
    let pagehide = {
        let pump = pump.clone();
        let queue = queue.clone();
        let state = state.clone();
        Closure::<dyn FnMut()>::new(move || flush(&pump, &queue, &state))
    };
    if window
        .add_event_listener_with_callback("pagehide", pagehide.as_ref().unchecked_ref())
        .is_err()
    {
        tracing::debug!(target: "diag_upload", "pagehide listener refused");
    }
    pump.hold_browser_listener(Box::new(interval));
    pump.hold_browser_listener(Box::new(pagehide));
}

fn flush(pump: &Pump, queue: &DiagUploadQueue, state: &Rc<SendState>) {
    if state.in_flight.get() || queue.is_empty() || js_sys::Date::now() < state.next_send_ms.get() {
        return;
    }
    let entries = queue.drain(BATCH_SIZE);
    if entries.is_empty() {
        return;
    }
    state.in_flight.set(true);
    let rpc = pump.rpc();
    let queue = queue.clone();
    let state = state.clone();
    let send = async move {
        let request = DiagDebugLogBatch { entries };
        match rpc.call(&request).await {
            Ok(_) => state.succeeded(),
            Err(error) => {
                queue.requeue_front(request.entries);
                state.failed(js_sys::Date::now());
                tracing::debug!(target: "diag_upload", %error, failures = state.failures.get(),
                    "diagnostic upload failed; batch requeued");
            }
        }
        state.in_flight.set(false);
    };
    wasm_bindgen_futures::spawn_local(
        send.instrument(tracing::info_span!(target: SEND_SPAN_TARGET, "diagnostic upload")),
    );
}
