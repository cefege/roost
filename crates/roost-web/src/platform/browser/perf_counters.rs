//! The document's performance counters: running long-task totals from a
//! `longtask` observer installed once at app start, and a bounded ring of
//! input→echo round trips fed by the pane's input dispatch and frame paint.
//! Read by the smoke `perfProbe` member and zeroed by `resetPerfCounters`; a
//! main-thread stall long enough to freeze the UI is logged as it happens.
//! Ports `apps/web/src/browser/leakWatch.ts` and the RTT stamps of
//! `apps/web/src/client/carriers/terminal-input-lanes.ts`.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};

/// How many round trips the ring keeps.
pub const INPUT_RTT_CAPACITY: usize = 500;

/// A round trip outside `(0, 5000)` ms is a stale stamp, not a felt delay.
const INPUT_RTT_CEILING_MS: f64 = 5_000.0;

/// A task this long is a perceptible whole-UI freeze.
const STALL_MS: f64 = 200.0;

/// One stall log per burst.
const STALL_THROTTLE_MS: f64 = 10_000.0;

/// Whether the long-task totals mean anything. Unsupported is reported as
/// such, never as a passing zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LongTaskState {
    Available,
    #[default]
    Unavailable,
}

impl LongTaskState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Everything the counters hold.
#[derive(Debug, Default)]
pub struct PerfCounters {
    pub long_task_state: LongTaskState,
    pub long_task_count: u64,
    pub long_task_ms: f64,
    input_rtt: VecDeque<f64>,
    /// When each session last sent input, consumed by its next painted frame.
    input_sent_at: BTreeMap<String, f64>,
    last_stall_ms: Option<f64>,
}

impl PerfCounters {
    /// Count one long task. Answers whether it is a stall to log now: long
    /// enough to freeze the UI and not inside the previous stall's throttle.
    pub fn record_long_task(&mut self, duration_ms: f64, now_ms: f64) -> bool {
        self.long_task_count += 1;
        self.long_task_ms += duration_ms;
        if duration_ms < STALL_MS {
            return false;
        }
        if self
            .last_stall_ms
            .is_some_and(|last| now_ms - last < STALL_THROTTLE_MS)
        {
            return false;
        }
        self.last_stall_ms = Some(now_ms);
        true
    }

    /// Zero the long-task totals, opening a fresh measurement window. The RTT
    /// ring is a trajectory, not a window, and is left alone.
    pub fn reset(&mut self) {
        self.long_task_count = 0;
        self.long_task_ms = 0.0;
    }

    /// `session_id` just sent input.
    pub fn note_input_sent(&mut self, session_id: &str, now_ms: f64) {
        self.input_sent_at.insert(session_id.to_owned(), now_ms);
    }

    /// `session_id` painted a frame: a pending send stamp becomes one round
    /// trip, and is consumed so the next frame without new input adds none.
    pub fn note_frame_painted(&mut self, session_id: &str, now_ms: f64) {
        let Some(sent_at) = self.input_sent_at.remove(session_id) else {
            return;
        };
        let round_trip = now_ms - sent_at;
        if !(round_trip > 0.0 && round_trip < INPUT_RTT_CEILING_MS) {
            return;
        }
        self.input_rtt.push_back(round_trip);
        if self.input_rtt.len() > INPUT_RTT_CAPACITY {
            self.input_rtt.pop_front();
        }
    }

    /// `session_id`'s pane is gone; its stamp would never be consumed.
    pub fn forget_session(&mut self, session_id: &str) {
        self.input_sent_at.remove(session_id);
    }

    /// The `quantile` round trip in whole ms, or -1 with no samples.
    pub fn input_rtt_percentile(&self, quantile: f64) -> i64 {
        let mut sorted: Vec<f64> = self.input_rtt.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        let Some(last) = sorted.len().checked_sub(1) else {
            return -1;
        };
        let rank = ((quantile * sorted.len() as f64).floor() as usize).min(last);
        sorted.get(rank).map_or(-1, |value| value.round() as i64)
    }
}

thread_local! {
    /// This document's counters. Document-scoped like the `longtask` observer
    /// that feeds them: the observer, every pane and the smoke member share no
    /// owner short of the document itself.
    static DOCUMENT_PERF: RefCell<PerfCounters> = RefCell::new(PerfCounters::default());
}

/// Read or change this document's counters.
pub fn with_perf_counters<R>(access: impl FnOnce(&mut PerfCounters) -> R) -> R {
    DOCUMENT_PERF.with(|counters| access(&mut counters.borrow_mut()))
}

/// Observe `longtask` entries for the document's life. Once, from app start;
/// a browser without the entry type leaves the state `Unavailable`.
#[cfg(target_arch = "wasm32")]
pub fn install_long_task_watch() {
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;

    if !long_tasks_supported() {
        tracing::info!(target: "perf", "longtask entries unsupported; long-task totals unavailable");
        return;
    }
    let observe = Closure::<dyn FnMut(web_sys::PerformanceObserverEntryList)>::new(
        |list: web_sys::PerformanceObserverEntryList| {
            let now = crate::platform::browser::now_ms();
            for entry in list.get_entries().iter() {
                let Ok(entry) = entry.dyn_into::<web_sys::PerformanceEntry>() else {
                    continue;
                };
                let duration_ms = entry.duration();
                if with_perf_counters(|counters| counters.record_long_task(duration_ms, now)) {
                    tracing::warn!(target: "perf", duration_ms = duration_ms.round() as u64,
                        "perf.longtask_stall");
                }
            }
        },
    );
    let Ok(observer) = web_sys::PerformanceObserver::new(observe.as_ref().unchecked_ref()) else {
        tracing::info!(target: "perf", "longtask observer refused; long-task totals unavailable");
        return;
    };
    let entry_types = js_sys::Array::of1(&"longtask".into());
    observer.observe(&web_sys::PerformanceObserverInit::new(&entry_types));
    // The observer lives as long as the document, so its callback does too.
    observe.forget();
    with_perf_counters(|counters| counters.long_task_state = LongTaskState::Available);
    tracing::info!(target: "perf", "longtask observer installed");
}

/// Whether `PerformanceObserver.supportedEntryTypes` names `longtask`.
#[cfg(target_arch = "wasm32")]
fn long_tasks_supported() -> bool {
    let global = js_sys::global();
    let Ok(constructor) = js_sys::Reflect::get(&global, &"PerformanceObserver".into()) else {
        return false;
    };
    if constructor.is_undefined() {
        return false;
    }
    js_sys::Reflect::get(&constructor, &"supportedEntryTypes".into())
        .ok()
        .filter(js_sys::Array::is_array)
        .map(|types| js_sys::Array::from(&types))
        .is_some_and(|types| types.includes(&"longtask".into(), 0))
}
