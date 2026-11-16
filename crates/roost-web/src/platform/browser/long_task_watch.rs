//! The document's long-task watch: a `longtask` observer installed once at app
//! start that logs a main-thread stall long enough to freeze the UI, at most
//! once per burst. Called from the wasm entry; `StallThrottle` is the pure
//! decision, so the throttle is testable natively.

#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;

/// A task this long is a perceptible whole-UI freeze.
const STALL_MS: f64 = 200.0;

/// One stall log per burst.
const STALL_THROTTLE_MS: f64 = 10_000.0;

/// Which long tasks are stalls worth a log line.
#[derive(Debug, Default)]
pub struct StallThrottle {
    last_stall_ms: Option<f64>,
}

impl StallThrottle {
    /// Whether a task of `duration_ms` ending near `now_ms` is a stall to log
    /// now: long enough to freeze the UI and not inside the previous stall's
    /// throttle.
    pub fn should_log(&mut self, duration_ms: f64, now_ms: f64) -> bool {
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
}

/// Observe `longtask` entries for the document's life. Once, from app start;
/// a browser without the entry type logs that and watches nothing.
#[cfg(target_arch = "wasm32")]
pub fn install_long_task_watch() {
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;

    if !long_tasks_supported() {
        tracing::info!(target: "perf", "longtask entries unsupported; stalls are not logged");
        return;
    }
    let throttle = RefCell::new(StallThrottle::default());
    let observe = Closure::<dyn FnMut(web_sys::PerformanceObserverEntryList)>::new(
        move |list: web_sys::PerformanceObserverEntryList| {
            let now = crate::platform::browser::now_ms();
            for entry in list.get_entries().iter() {
                let Ok(entry) = entry.dyn_into::<web_sys::PerformanceEntry>() else {
                    continue;
                };
                let duration_ms = entry.duration();
                if throttle.borrow_mut().should_log(duration_ms, now) {
                    tracing::warn!(target: "perf", duration_ms = duration_ms.round() as u64,
                        "perf.longtask_stall");
                }
            }
        },
    );
    let Ok(observer) = web_sys::PerformanceObserver::new(observe.as_ref().unchecked_ref()) else {
        tracing::info!(target: "perf", "longtask observer refused; stalls are not logged");
        return;
    };
    let entry_types = js_sys::Array::of1(&"longtask".into());
    observer.observe(&web_sys::PerformanceObserverInit::new(&entry_types));
    // The observer lives as long as the document, so its callback does too.
    observe.forget();
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
