//! The document's own instrumentation: the phase-mark ring and the long-task
//! stall watch. Both are always on and document-scoped, because the questions
//! they answer ("how long did this cold navigation take to paint?", "did the
//! main thread freeze?") cannot be asked after the fact. The phase ring is
//! read through `window.__roostPhaseTimeline()`; stalls go to the log.

pub mod long_task_watch;
pub mod phase_marks;
pub mod sync_marks;

/// `performance.now()`, or 0 where the document has no clock.
#[cfg(target_arch = "wasm32")]
pub fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or(0.0, |performance| performance.now())
}
