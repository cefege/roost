//! The `perfProbe(sessionId)` answer: the document's long-task totals and
//! round-trip percentiles, the session's frame counts, and a DOM/heap sample.
//! Answered by `smoke::dispatch`; reads `platform::browser::perf_counters` and
//! the page. Ports `perfProbe` of `apps/web/src/smoke/smokeRuntimeControls.ts`
//! and `leakSample` of `apps/web/src/browser/leakWatch.ts`.

use js_sys::Reflect;
use serde_json::{Value, json};
use wasm_bindgen::JsValue;

use super::dom;
use crate::platform::browser::perf_counters::with_perf_counters;

/// The probe, with the session's `(frames, full_frames)` already read.
pub(super) fn perf_probe_json(cell_frames: u64, cell_full_frames: u64) -> Value {
    let (state, count, long_task_ms, rtt_p50, rtt_p95) = with_perf_counters(|counters| {
        (
            counters.long_task_state.as_str(),
            counters.long_task_count,
            counters.long_task_ms.round() as u64,
            counters.input_rtt_percentile(0.5),
            counters.input_rtt_percentile(0.95),
        )
    });
    json!({
        "longTaskState": state,
        "longTaskCount": count,
        "longTaskMs": long_task_ms,
        "cellFrames": cell_frames,
        "cellFullFrames": cell_full_frames,
        "domNodes": element_count("*"),
        "cellRows": element_count(".cell-row"),
        "heldSbRows": element_count(".cell-scrollback .cell-row"),
        "heapMb": heap_mb(),
        "inputRttP50": rtt_p50,
        "inputRttP95": rtt_p95,
    })
}

/// How many elements match `selector`, or -1 without a document.
fn element_count(selector: &str) -> i64 {
    dom::document()
        .and_then(|document| document.query_selector_all(selector).ok())
        .map_or(-1, |matches| i64::from(matches.length()))
}

/// `performance.memory.usedJSHeapSize` in whole MB, or -1 where the browser
/// does not expose it.
fn heap_mb() -> i64 {
    let Some(performance) = dom::window().and_then(|window| window.performance()) else {
        return -1;
    };
    Reflect::get(&performance, &JsValue::from_str("memory"))
        .ok()
        .filter(|memory| memory.is_object())
        .and_then(|memory| Reflect::get(&memory, &JsValue::from_str("usedJSHeapSize")).ok())
        .and_then(|used| used.as_f64())
        .map_or(-1, |used| (used / 1e6).round() as i64)
}
