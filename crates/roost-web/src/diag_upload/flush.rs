//! Sending queued browser diagnostics after coordinator authentication is ready.
//!
//! A single in-flight call preserves queue order; failed batches return to the
//! front so transient transport loss does not silently discard the evidence.

use std::cell::Cell;
use std::rc::Rc;

use roost_client_core::client::rpc::calls::diag_debug_log_batch::DiagDebugLogBatch;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

use super::DiagUploadQueue;
use crate::pump::Pump;

const FLUSH_INTERVAL_MS: i32 = 1_000;
const BATCH_SIZE: usize = 64;

pub(crate) fn install(pump: &Pump, queue: DiagUploadQueue) {
    let Some(window) = web_sys::window() else {
        tracing::error!(target: "diag_upload", "no window: diagnostic uploads not installed");
        return;
    };
    let in_flight = Rc::new(Cell::new(false));
    let interval = {
        let pump = pump.clone();
        let queue = queue.clone();
        let in_flight = in_flight.clone();
        Closure::<dyn FnMut()>::new(move || flush(&pump, &queue, &in_flight))
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
        let in_flight = in_flight.clone();
        Closure::<dyn FnMut()>::new(move || flush(&pump, &queue, &in_flight))
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

fn flush(pump: &Pump, queue: &DiagUploadQueue, in_flight: &Rc<Cell<bool>>) {
    if in_flight.get() || queue.is_empty() {
        return;
    }
    let entries = queue.drain(BATCH_SIZE);
    if entries.is_empty() {
        return;
    }
    in_flight.set(true);
    let rpc = pump.rpc();
    let queue = queue.clone();
    let in_flight = in_flight.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let request = DiagDebugLogBatch { entries };
        let result = rpc.call(&request).await;
        if let Err(error) = result {
            queue.requeue_front(request.entries);
            tracing::debug!(target: "diag_upload", %error, "diagnostic upload failed; batch requeued");
        }
        in_flight.set(false);
    });
}
