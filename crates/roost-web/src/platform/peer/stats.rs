//! What the browser's own `getStats` report says about one open peer: which
//! kind of address its selected candidate pair reached the far end at.
//!
//! Owned by `platform::peer`, called by `BrowserPeer::measure_attempt`. `getStats`
//! is a PROMISE, so the read is started from the pump's tick and its answer is
//! pushed into the same bounded sink every other browser callback writes: a
//! callback that awaited would have to hold the pump's borrow across a
//! network-shaped wait, and a callback that reached for the store would re-enter a
//! `RefCell` on a single-threaded loop.
//!
//! Nothing here invents a value. A report with no selected pair answers no
//! candidate, and that is what the diagnostic publishes for a peer that has not
//! connected — a reader can tell "not measured" from a guessed kind.
//!
//! `RTCStatsReport` is map-like, not a plain object: its entries are reached
//! through `forEach` and `get` and NOT through property reads, so a host that read
//! `report.selectedPairId` would silently measure nothing.

#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(target_arch = "wasm32")]
use std::collections::BTreeMap;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

#[cfg(target_arch = "wasm32")]
use js_sys::{Array, Function, Promise, Reflect};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast as _;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsValue;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::closure::Closure;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::carriers::CandidateType;

#[cfg(target_arch = "wasm32")]
use super::events::{PeerEvent, PeerEventSink, PeerMeasurement};

/// Start one stats read for a peer, and record its answer in the sink.
///
/// The attempt's identity rides along because a report that settles after the peer
/// closed names a connection nothing holds: the sink is drained by attempt id,
/// and an event naming a retired attempt is discarded there rather than applied
/// to whatever took its place.
#[cfg(target_arch = "wasm32")]
pub(super) fn measure(connection: &JsValue, attempt_id: u64, sink: &PeerEventSink) {
    let Some(promise) = invoke(connection, "getStats", &Array::new()) else {
        return;
    };
    let promise: Promise = promise.into();
    let sink = sink.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let Ok(report) = JsFuture::from(promise).await else {
            return;
        };
        sink.record(PeerEvent::Measured {
            attempt_id,
            measurement: selected_pair(&report),
        });
    });
}

/// The bytes the browser itself is holding for a peer's channels.
///
/// A property read, not a report query: it is the number the send path is bounded
/// by, and a write that waited on a `getStats` round trip would be a keystroke
/// queued behind diagnostics.
#[cfg(target_arch = "wasm32")]
pub(super) fn buffered_bytes(channels: &BTreeMap<u16, JsValue>) -> u64 {
    channels
        .values()
        .map(|channel| {
            Reflect::get(channel, &JsValue::from_str("bufferedAmount"))
                .ok()
                .and_then(|value| value.as_f64())
                .map_or(0, |amount| {
                    if amount.is_finite() && amount > 0.0 {
                        amount as u64
                    } else {
                        0
                    }
                })
        })
        .sum()
}

/// The candidate kind of the pair the browser selected.
#[cfg(target_arch = "wasm32")]
fn selected_pair(report: &JsValue) -> PeerMeasurement {
    let entries = report_entries(report);
    let pair = entries
        .iter()
        .find(|entry| flag_field(entry, "selected"))
        .or_else(|| {
            entries.iter().find(|entry| {
                flag_field(entry, "nominated")
                    && text_field(entry, "state").as_deref() == Some("succeeded")
            })
        })
        .cloned();
    let Some(pair) = pair else {
        return PeerMeasurement::default();
    };
    PeerMeasurement {
        candidate_type: remote_candidate_type(report, &pair),
    }
}

/// Every entry a report holds, read through `forEach`.
///
/// `forEach` because it is the one method a map-like object is required to have,
/// and its callback is a `Closure` this file owns for the length of the call.
#[cfg(target_arch = "wasm32")]
fn report_entries(report: &JsValue) -> Vec<JsValue> {
    let Some(for_each) = method(report, "forEach") else {
        return Vec::new();
    };
    let collected: Rc<RefCell<Vec<JsValue>>> = Rc::default();
    let sink = Rc::clone(&collected);
    // `Map.prototype.forEach` hands its callback `(value, key, map)`, and a
    // `Closure` whose arity does not match what it is called with throws the
    // first time a browser reports anything.
    let callback = Closure::<dyn FnMut(JsValue, JsValue, JsValue)>::new(
        move |value: JsValue, _key: JsValue, _report: JsValue| {
            if value.is_object() {
                sink.borrow_mut().push(value);
            }
        },
    );
    let arguments = Array::of2(callback.as_ref().unchecked_ref(), &JsValue::NULL);
    if for_each.apply(report, &arguments).is_err() {
        return Vec::new();
    }
    let entries = collected.borrow().clone();
    entries
}

/// The kind of address the pair's far end was reached at.
#[cfg(target_arch = "wasm32")]
fn remote_candidate_type(report: &JsValue, pair: &JsValue) -> CandidateType {
    let Some(remote_id) = text_field(pair, "remoteCandidateId") else {
        return CandidateType::None;
    };
    let Some(get) = method(report, "get") else {
        return CandidateType::None;
    };
    let arguments = Array::of1(&JsValue::from_str(&remote_id));
    let Ok(remote) = get.apply(report, &arguments) else {
        return CandidateType::None;
    };
    if !remote.is_object() {
        return CandidateType::None;
    }
    match text_field(&remote, "candidateType").as_deref() {
        Some("host") => CandidateType::Host,
        Some("srflx") => CandidateType::Srflx,
        Some("prflx") => CandidateType::Prflx,
        _ => CandidateType::None,
    }
}

/// One named method on a browser object, or its absence.
#[cfg(target_arch = "wasm32")]
fn method(object: &JsValue, name: &str) -> Option<Function> {
    Reflect::get(object, &JsValue::from_str(name))
        .ok()?
        .dyn_into::<Function>()
        .ok()
}

/// One call through a named method, and the refusal as a refusal.
#[cfg(target_arch = "wasm32")]
fn invoke(object: &JsValue, name: &str, arguments: &Array) -> Option<JsValue> {
    method(object, name)?.apply(object, arguments).ok()
}

/// A report field that is a string.
#[cfg(target_arch = "wasm32")]
fn text_field(record: &JsValue, name: &str) -> Option<String> {
    Reflect::get(record, &JsValue::from_str(name))
        .ok()?
        .as_string()
}

/// A report field that is a boolean.
#[cfg(target_arch = "wasm32")]
fn flag_field(record: &JsValue, name: &str) -> bool {
    Reflect::get(record, &JsValue::from_str(name))
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}
