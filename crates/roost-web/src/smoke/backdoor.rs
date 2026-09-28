//! Installing `window.__smoke`: the gate (`localStorage.roostSmoke === "1"`),
//! the backdoor's own state, and one JS function per `SmokeApi` member that
//! parses its arguments natively (`smoke::call`) and answers a value, a thrown
//! `Error`, or a Promise. wasm32 only; called once by `App` in a smoke build.
//! Ports `apps/web/src/smoke/smoke.ts` and the persisted-arm half of
//! `apps/web/src/store/terminal-stream-state.ts:55-79`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::pin::Pin;
use std::rc::Rc;

use js_sys::{JSON, Object, Promise, Reflect};
use roost_client_core::KeyValueStore as _;
use roost_client_core::terminal::input::smoke_observer::SmokeInputObserver;
use serde_json::{Value, json};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::future_to_promise;
use web_sys::{KeyboardEvent, Storage};

use super::browser_snapshot::GeometryProofs;
use super::call::{Answer, SMOKE_METHODS, parse_call};
use super::created_resources::{CREATED_RESOURCES_KEY, CreatedResources};
use super::dom;
use super::dom_hold::DomHolds;
use super::timing::TimingLedger;
use crate::components::terminal::pane_registry::PaneRegistry;
use crate::platform::{LocalStorageKeyValueStore, SessionStorageKeyValueStore};
use crate::pump::Pump;

/// The `sessionStorage` prefix of a renderer-drop arm that survives a reload.
const DROP_CELL_KEY_PREFIX: &str = "roostSmoke.dropCell.";

/// What one member call answers.
pub(super) enum Reply {
    /// Answered now: a value (`None` is `undefined`) or an error.
    Now(Result<Option<Value>, String>),
    /// Answered when the future settles.
    Later(Pin<Box<dyn Future<Output = Result<Option<Value>, String>>>>),
}

/// One in-flight timing measurement's key listener, keyed by its timing id.
type TimingListeners = BTreeMap<String, Closure<dyn FnMut(KeyboardEvent)>>;
/// The backdoor's state; one per document.
pub(super) struct SmokeBackdoor {
    pub(super) pump: Pump,
    pub(super) panes: PaneRegistry,
    pub(super) created: RefCell<CreatedResources>,
    pub(super) timings: RefCell<TimingLedger>,
    pub(super) holds: RefCell<DomHolds>,
    pub(super) geometry_proofs: RefCell<GeometryProofs>,
    timing_listeners: RefCell<TimingListeners>,
    session_storage: Rc<SessionStorageKeyValueStore>,
}

/// Install `window.__smoke` when this document opted in.
pub fn install_smoke_backdoor(pump: &Pump, panes: &PaneRegistry) {
    if LocalStorageKeyValueStore::new()
        .get("roostSmoke")
        .as_deref()
        != Some("1")
    {
        return;
    }
    let Some(window) = dom::window() else {
        return;
    };
    let session_storage = Rc::new(SessionStorageKeyValueStore::new());
    let created = CreatedResources::restore(session_storage.get(CREATED_RESOURCES_KEY).as_deref());
    let backdoor = Rc::new(SmokeBackdoor {
        pump: pump.clone(),
        panes: panes.clone(),
        created: RefCell::new(created),
        timings: RefCell::new(TimingLedger::default()),
        holds: RefCell::new(DomHolds::default()),
        geometry_proofs: RefCell::new(GeometryProofs::default()),
        timing_listeners: RefCell::new(BTreeMap::new()),
        session_storage,
    });
    pump.core().borrow_mut().store_mut().input.smoke_observer = Some(SmokeInputObserver::new());
    backdoor.restore_renderer_drops(&window);

    let api = Object::new();
    for (name, answer) in SMOKE_METHODS {
        let member = bind_member(&backdoor, name, answer);
        if Reflect::set(&api, &JsValue::from_str(name), &member).is_err() {
            tracing::error!(target: "smoke", member = name, "__smoke member refused");
        }
    }
    if Reflect::set(&window, &JsValue::from_str("__smoke"), &api).is_err() {
        tracing::error!(target: "smoke", "window refused __smoke");
        return;
    }
    tracing::info!(target: "smoke", members = SMOKE_METHODS.len(), "backdoor installed via window.__smoke");
}

fn bind_member(backdoor: &Rc<SmokeBackdoor>, name: &'static str, answer: Answer) -> JsValue {
    let backdoor = Rc::clone(backdoor);
    let member = move |first: JsValue, second: JsValue, third: JsValue, fourth: JsValue| {
        let args = [first, second, third, fourth].map(|arg| js_to_json(&arg));
        let reply = match parse_call(name, &args) {
            Ok(call) => backdoor.answer(call),
            Err(message) => Reply::Now(Err(message)),
        };
        match (answer, reply) {
            (Answer::Promise, Reply::Later(future)) => {
                Ok(JsValue::from(future_to_promise(async move {
                    json_to_js(future.await)
                })))
            }
            (Answer::Promise, Reply::Now(result)) => Ok(JsValue::from(match json_to_js(result) {
                Ok(value) => Promise::resolve(&value),
                Err(error) => Promise::reject(&error),
            })),
            (Answer::Sync, Reply::Now(result)) => json_to_js(result),
            (Answer::Sync, Reply::Later(_)) => {
                json_to_js(Err(format!("__smoke.{name} answered asynchronously")))
            }
        }
    };
    Closure::<dyn Fn(JsValue, JsValue, JsValue, JsValue) -> Result<JsValue, JsValue>>::new(member)
        .into_js_value()
}

/// A JS argument as JSON; `undefined`, `null` and anything unserializable are `Null`.
fn js_to_json(value: &JsValue) -> Value {
    if value.is_undefined() || value.is_null() {
        return Value::Null;
    }
    JSON::stringify(value)
        .ok()
        .and_then(|text| text.as_string())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

fn json_to_js(result: Result<Option<Value>, String>) -> Result<JsValue, JsValue> {
    match result {
        Ok(None) => Ok(JsValue::UNDEFINED),
        Ok(Some(value)) => JSON::parse(&value.to_string()),
        Err(message) => Err(js_sys::Error::new(&message).into()),
    }
}

impl SmokeBackdoor {
    /// Write the created-resource ledger through to `sessionStorage`.
    pub(super) fn persist_created(&self) {
        let encoded = self.created.borrow().encode();
        self.session_storage.set(CREATED_RESOURCES_KEY, &encoded);
    }

    /// Arm a renderer-frame drop for `session_id` that also survives a reload.
    pub(super) fn arm_renderer_drop(&self, session_id: &str) {
        self.panes.arm_drop_next_frame(session_id);
        self.session_storage
            .set(&format!("{DROP_CELL_KEY_PREFIX}{session_id}"), "1");
    }

    /// Re-arm the drops a previous document persisted, and retire each
    /// persisted arm when the pane consumes it.
    fn restore_renderer_drops(&self, window: &web_sys::Window) {
        let storage: Option<Storage> = window.session_storage().ok().flatten();
        let persisted: Vec<String> = storage
            .as_ref()
            .map(|storage| {
                let length = storage.length().unwrap_or(0);
                (0..length)
                    .filter_map(|index| storage.key(index).ok().flatten())
                    .filter(|key| key.starts_with(DROP_CELL_KEY_PREFIX))
                    .collect()
            })
            .unwrap_or_default();
        for key in persisted {
            if self.session_storage.get(&key).as_deref() == Some("1") {
                self.panes
                    .arm_drop_next_frame(&key[DROP_CELL_KEY_PREFIX.len()..]);
            }
        }
        let storage = Rc::clone(&self.session_storage);
        self.panes
            .set_frame_drop_taken_hook(Rc::new(move |session_id: &str| {
                storage.remove(&format!("{DROP_CELL_KEY_PREFIX}{session_id}"));
            }));
    }

    /// Keep a `trusted_key` timing's capture-phase keydown listener.
    pub(super) fn install_timing_listener(
        &self,
        id: &str,
        listener: Closure<dyn FnMut(KeyboardEvent)>,
    ) {
        let attached = dom::document().is_some_and(|document| {
            document
                .add_event_listener_with_callback_and_bool(
                    "keydown",
                    listener.as_ref().unchecked_ref(),
                    true,
                )
                .is_ok()
        });
        if !attached {
            tracing::warn!(target: "smoke", id, "trusted keydown listener refused");
        }
        self.timing_listeners
            .borrow_mut()
            .insert(id.to_owned(), listener);
    }

    /// Detach and drop a timing's keydown listener, if it has one.
    pub(super) fn drop_timing_listener(&self, id: &str) {
        let Some(listener) = self.timing_listeners.borrow_mut().remove(id) else {
            return;
        };
        if let Some(document) = dom::document() {
            let _ = document.remove_event_listener_with_callback_and_bool(
                "keydown",
                listener.as_ref().unchecked_ref(),
                true,
            );
        }
    }

    /// The diagnosis a marker wait that timed out answers with.
    pub(super) fn marker_timeout_message(
        &self,
        session_id: &str,
        marker: &str,
        timeout_ms: f64,
    ) -> String {
        let slot = dom::slot(session_id);
        let core = self.pump.core();
        let core = core.borrow();
        let replica = core.store().terminal(session_id);
        let detail = json!({
            "in_dom": slot.as_ref().is_some_and(|slot| dom::text_of(slot).contains(marker)),
            "rows": slot.as_ref().map_or(0, |slot| dom::all(slot, ".cell-row").len()),
            "wire": replica.map(|replica| json!({
                "stream_id": replica.wire_stream_id,
                "grid_epoch": replica.wire_grid_epoch,
                "seq": replica.wire_seq,
            })),
            "baseline_ready": replica.is_some_and(|replica| replica.baseline_ready()),
            "pane_mounted": self.panes.mount_id(session_id).is_some(),
        });
        format!(
            "marker was not visibly painted within {timeout_ms}ms: {session_id} {} {detail}",
            Value::String(marker.to_owned())
        )
    }
}
