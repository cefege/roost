//! The document's own instrumentation: the phase-mark ring and the
//! performance counters. Both are always on and document-scoped, because the
//! questions they answer ("how long did this cold navigation take to paint?",
//! "how much of that window was main-thread jank?") cannot be asked after the
//! fact. Read by the smoke members; ports `apps/web/src/browser/diag.ts`'s
//! phase recorder and `apps/web/src/browser/leakWatch.ts`.

pub mod perf_counters;
pub mod phase_marks;
pub mod sync_marks;

/// `performance.now()`, or 0 where the document has no clock.
#[cfg(target_arch = "wasm32")]
pub fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or(0.0, |performance| performance.now())
}
