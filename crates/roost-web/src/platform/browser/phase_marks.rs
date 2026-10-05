//! The document's phase recorder: a fixed 256-slot ring of bootstrap and
//! terminal phase marks, always on, so an ordinary cold navigation can be
//! measured without enabling diagnostics first. Marked from the app entry,
//! the pump's Sync delivery, the pane mount and the smoke paint proofs; read
//! through `window.__roostPhaseTimeline()`. Ports `apps/web/src/browser/diag.ts`
//! (`markPhase`, `markPhaseOnce`, `phaseTimeline`).

use std::cell::RefCell;

use serde_json::{Map, Value, json};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast as _;

/// How many marks the ring keeps. Small and fixed: the recorder must never
/// grow with reconnects or session churn.
pub const PHASE_MARK_CAPACITY: usize = 256;

/// Every phase a mark may name, spelled as the timeline reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseName {
    ModuleStart,
    IdentityComplete,
    SyncSubscribed,
    SnapshotComplete,
    SnapshotApplied,
    SessionsListPublish,
    TerminalMount,
    ViewportEnqueue,
    ViewportAccept,
    FirstCellReceive,
    FirstCellApply,
    MarkerPresented,
    CursorPresented,
}

impl PhaseName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModuleStart => "module_start",
            Self::IdentityComplete => "identity_complete",
            Self::SyncSubscribed => "sync_subscribed",
            Self::SnapshotComplete => "snapshot_complete",
            Self::SnapshotApplied => "snapshot_applied",
            Self::SessionsListPublish => "sessions_list_publish",
            Self::TerminalMount => "terminal_mount",
            Self::ViewportEnqueue => "viewport_enqueue",
            Self::ViewportAccept => "viewport_accept",
            Self::FirstCellReceive => "first_cell_receive",
            Self::FirstCellApply => "first_cell_apply",
            Self::MarkerPresented => "marker_presented",
            Self::CursorPresented => "cursor_presented",
        }
    }
}

/// The document clocks a mark is placed against. Both origins are fixed for a
/// document's life; only `monotonic_ms` moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhaseClock {
    /// `performance.now()`.
    pub monotonic_ms: f64,
    /// `performance.timeOrigin`.
    pub time_origin_epoch_ms: f64,
    /// The time origin plus the navigation entry's `startTime`.
    pub navigation_start_epoch_ms: f64,
}

/// One recorded mark.
#[derive(Debug, Clone, PartialEq)]
pub struct PhaseMark {
    pub index: u64,
    pub name: PhaseName,
    pub monotonic_ms: f64,
    pub epoch_ms: f64,
    pub since_navigation_ms: f64,
    pub once_key: Option<String>,
    /// Scalar values only: a string, a number, a boolean or `null`.
    pub detail: Map<String, Value>,
}

/// The ring and its write counter.
#[derive(Debug, Clone)]
pub struct PhaseRing {
    slots: Vec<Option<PhaseMark>>,
    writes: u64,
}

impl Default for PhaseRing {
    fn default() -> Self {
        Self {
            slots: vec![None; PHASE_MARK_CAPACITY],
            writes: 0,
        }
    }
}

impl PhaseRing {
    /// Record one mark, overwriting the oldest once the ring is full.
    pub fn mark(&mut self, name: PhaseName, detail: &[(&str, Value)], clock: PhaseClock) -> u64 {
        self.write(name, None, detail, clock)
    }

    /// Record `name` for `once_key` unless a mark for the pair is still in the
    /// ring; answers the index of whichever mark stands.
    pub fn mark_once(
        &mut self,
        name: PhaseName,
        once_key: &str,
        detail: &[(&str, Value)],
        clock: PhaseClock,
    ) -> u64 {
        if let Some(existing) = self.find_mark(name, once_key) {
            return existing.index;
        }
        self.write(name, Some(once_key.to_owned()), detail, clock)
    }

    /// The timeline, oldest mark first, under v2's keys. `driver_epoch_ms` is
    /// the driver's pre-navigation stamp, reported only when it is finite.
    pub fn timeline_json(&self, clock: PhaseClock, driver_epoch_ms: Option<f64>) -> Value {
        let marks: Vec<Value> = self.retained().map(mark_json).collect();
        json!({
            "capacity": PHASE_MARK_CAPACITY,
            "dropped": self.writes.saturating_sub(PHASE_MARK_CAPACITY as u64),
            "timeOriginEpochMs": clock.time_origin_epoch_ms,
            "navigationStartEpochMs": clock.navigation_start_epoch_ms,
            "driverBeforeNavigationEpochMs": driver_epoch_ms.filter(|epoch| epoch.is_finite()),
            "marks": marks,
        })
    }

    fn write(
        &mut self,
        name: PhaseName,
        once_key: Option<String>,
        detail: &[(&str, Value)],
        clock: PhaseClock,
    ) -> u64 {
        let index = self.writes;
        let epoch_ms = clock.time_origin_epoch_ms + clock.monotonic_ms;
        let mark = PhaseMark {
            index,
            name,
            monotonic_ms: clock.monotonic_ms,
            epoch_ms,
            since_navigation_ms: epoch_ms - clock.navigation_start_epoch_ms,
            once_key,
            detail: detail
                .iter()
                .filter(|(_, value)| !value.is_array() && !value.is_object())
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect(),
        };
        if let Some(slot) = self.slots.get_mut(slot_of(index)) {
            *slot = Some(mark);
        }
        self.writes += 1;
        index
    }

    /// Whether a mark for `name` and `once_key` is still in the ring.
    pub fn has_mark(&self, name: PhaseName, once_key: &str) -> bool {
        self.find_mark(name, once_key).is_some()
    }

    fn find_mark(&self, name: PhaseName, once_key: &str) -> Option<&PhaseMark> {
        self.retained()
            .find(|mark| mark.name == name && mark.once_key.as_deref() == Some(once_key))
    }

    /// The retained marks, oldest first.
    fn retained(&self) -> impl Iterator<Item = &PhaseMark> {
        let first = self.writes.saturating_sub(PHASE_MARK_CAPACITY as u64);
        (first..self.writes).filter_map(|index| self.slots.get(slot_of(index))?.as_ref())
    }
}

fn slot_of(index: u64) -> usize {
    (index % PHASE_MARK_CAPACITY as u64) as usize
}

fn mark_json(mark: &PhaseMark) -> Value {
    let mut value = json!({
        "index": mark.index,
        "name": mark.name.as_str(),
        "monotonicMs": mark.monotonic_ms,
        "epochMs": mark.epoch_ms,
        "sinceNavigationMs": mark.since_navigation_ms,
        "detail": mark.detail,
    });
    if let (Some(key), Some(object)) = (&mark.once_key, value.as_object_mut()) {
        object.insert("onceKey".to_owned(), Value::String(key.clone()));
    }
    value
}

thread_local! {
    /// This document's ring. Document-scoped like `performance.timeOrigin`:
    /// the mark sites (app entry, Sync delivery, pane mount, paint proofs)
    /// share no owner short of the document itself.
    static DOCUMENT_PHASES: RefCell<PhaseRing> = RefCell::new(PhaseRing::default());
}

/// Record one phase mark in this document's ring.
pub fn mark_phase(name: PhaseName, detail: &[(&str, Value)]) {
    let clock = document_clock();
    DOCUMENT_PHASES.with(|ring| {
        ring.borrow_mut().mark(name, detail, clock);
    });
}

/// Record the first `name` for `once_key` in this document's ring. A repeat
/// returns before reading the document clock, which costs a navigation-entry
/// lookup.
pub fn mark_phase_once(name: PhaseName, once_key: &str, detail: &[(&str, Value)]) {
    if phase_marked_once(name, once_key) {
        return;
    }
    let clock = document_clock();
    DOCUMENT_PHASES.with(|ring| {
        ring.borrow_mut().mark_once(name, once_key, detail, clock);
    });
}

/// Whether this document's ring still holds the `name` mark for `once_key`.
pub fn phase_marked_once(name: PhaseName, once_key: &str) -> bool {
    DOCUMENT_PHASES.with(|ring| ring.borrow().has_mark(name, once_key))
}

/// Record `name` for one session, the detail every per-pane mark carries.
pub fn mark_session_phase(name: PhaseName, session_id: &str) {
    mark_phase(name, &[("sessionId", Value::String(session_id.to_owned()))]);
}

/// This document's timeline, as the smoke `phaseTimeline` member answers it.
pub fn phase_timeline() -> Value {
    let clock = document_clock();
    DOCUMENT_PHASES.with(|ring| ring.borrow().timeline_json(clock, driver_epoch_ms()))
}

/// Publish [`phase_timeline`] as `window.__roostPhaseTimeline()`, returning the
/// timeline as JSON text, so a driver measuring a navigation reads the same
/// ring the marks were written to. Installed once, at the wasm entry.
#[cfg(target_arch = "wasm32")]
pub fn install_phase_timeline_member() {
    use wasm_bindgen::JsValue;
    use wasm_bindgen::closure::Closure;

    let Some(window) = web_sys::window() else {
        return;
    };
    let timeline =
        Closure::<dyn Fn() -> JsValue>::new(|| JsValue::from_str(&phase_timeline().to_string()));
    if js_sys::Reflect::set(&window, &"__roostPhaseTimeline".into(), timeline.as_ref()).is_err() {
        tracing::warn!(target: "perf", "the browser refused the phase timeline member");
    }
    // The member lives as long as the document does.
    timeline.forget();
}

#[cfg(target_arch = "wasm32")]
fn document_clock() -> PhaseClock {
    let Some(performance) = web_sys::window().and_then(|window| window.performance()) else {
        return PhaseClock {
            monotonic_ms: 0.0,
            time_origin_epoch_ms: js_sys::Date::now(),
            navigation_start_epoch_ms: js_sys::Date::now(),
        };
    };
    let time_origin = performance.time_origin();
    let navigation_start = performance
        .get_entries_by_type("navigation")
        .get(0)
        .dyn_into::<web_sys::PerformanceEntry>()
        .map_or(0.0, |entry| entry.start_time());
    PhaseClock {
        monotonic_ms: performance.now(),
        time_origin_epoch_ms: time_origin,
        navigation_start_epoch_ms: time_origin + navigation_start,
    }
}

/// A native build has no document, so its clocks all start at zero.
#[cfg(not(target_arch = "wasm32"))]
fn document_clock() -> PhaseClock {
    PhaseClock {
        monotonic_ms: 0.0,
        time_origin_epoch_ms: 0.0,
        navigation_start_epoch_ms: 0.0,
    }
}

/// `window.__roostDriverBeforeNavigationEpochMs`, which a driver's init script
/// sets before the navigation it times.
#[cfg(target_arch = "wasm32")]
fn driver_epoch_ms() -> Option<f64> {
    let window = web_sys::window()?;
    js_sys::Reflect::get(&window, &"__roostDriverBeforeNavigationEpochMs".into())
        .ok()?
        .as_f64()
}

#[cfg(not(target_arch = "wasm32"))]
fn driver_epoch_ms() -> Option<f64> {
    None
}
