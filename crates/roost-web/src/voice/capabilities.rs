//! What this browser actually exposes, asked once per recording.
//!
//! The engine decision in `super::engine` is pure; this is the half that has to
//! look at the window, and it is separate because a browser without an
//! `AudioContext` and a page served over plain http are different refusals with
//! different fixes. Ports the probing half of
//! `apps/web/src/components/MobileVoiceInput.tsx` (`webSupported`,
//! `deepgramSupported`) and of `voiceState.ts` (`probeMicPermission`).

use std::cell::Cell;

use js_sys::{Function, Object, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{MediaDevices, PermissionState, Permissions, Window};

use super::engine::EngineInputs;
use super::ownership::MicPermission;

thread_local! {
    /// The microphone permission latch a Permissions probe writes into.
    ///
    /// A `thread_local` rather than a signal because the probe answers on a
    /// microtask, with no component to re-render, and the only reader is the next
    /// recording's start gate.
    static GRANTED: Cell<bool> = const { Cell::new(false) };
}

/// Whether the microphone permission has already been granted, latching it.
///
/// A resolved non-granted answer never clears the latch: `prompt` is what the
/// Permissions API reports before the first grant, and treating that as a
/// refusal would refuse the operator's own first tap.
#[must_use]
pub fn mic_permission() -> MicPermission {
    let mut permission = MicPermission::default();
    permission.observe_state(GRANTED.get());
    permission
}

/// Record that a permission has been granted by any path.
pub fn note_mic_granted() {
    GRANTED.set(true);
}

/// The window, or `None` when there is none.
fn window() -> Option<Window> {
    web_sys::window()
}

/// Whether the page may ask for a microphone at all.
///
/// A microphone request from a page served over plain http is refused by every
/// browser, so this is asked before the engine is chosen rather than discovered
/// later as a permission error.
#[must_use]
pub fn is_secure_context() -> bool {
    window().is_some_and(|window| window.is_secure_context())
}

/// Read what the browser exposes, with the coordinator's stored key folded in.
#[must_use]
pub fn probe(deepgram_configured: bool) -> EngineInputs {
    EngineInputs {
        deepgram_configured,
        web_speech_supported: web_speech_supported(),
        deepgram_supported: deepgram_supported(),
    }
}

/// Whether `SpeechRecognition` or its prefixed form is on the global.
#[must_use]
pub fn web_speech_supported() -> bool {
    has_global("SpeechRecognition") || has_global("webkitSpeechRecognition")
}

fn has_global(name: &str) -> bool {
    Reflect::get(&js_sys::global(), &JsValue::from_str(name))
        .is_ok_and(|value| !value.is_undefined())
}

/// Whether the Deepgram transport is complete: a socket to open, a device to
/// record, and an audio graph to read frames from.
#[must_use]
pub fn deepgram_supported() -> bool {
    has_global("WebSocket") && media_devices().is_some() && audio_context_constructor().is_some()
}

/// `navigator.mediaDevices`, when the browser exposes one.
///
/// Duck-typed rather than `instanceof`-checked: a page may stand in for the
/// device API with a plain object carrying the same surface — a test harness
/// faking a microphone, a webview, a polyfill — and every call this crate makes
/// on it is a plain property read or method call, which is exactly what
/// web-sys itself does with the wrapper. An `instanceof` check would refuse a
/// working microphone and silently demote dictation to the other engine.
#[must_use]
pub fn media_devices() -> Option<MediaDevices> {
    let navigator = window()?.navigator();
    let devices = Reflect::get(navigator.as_ref(), &JsValue::from_str("mediaDevices")).ok()?;
    has_callable(&devices, "getUserMedia").then(|| stand_in::<MediaDevices>(devices))
}

/// Wrap an object that answers to a web-sys type's surface without being an
/// instance of it.
#[must_use]
pub fn stand_in<T: JsCast>(value: JsValue) -> T {
    value.unchecked_into()
}

/// Whether `value` carries a callable member by that name.
pub fn has_callable(value: &JsValue, name: &str) -> bool {
    Reflect::get(value, &JsValue::from_str(name)).is_ok_and(|member| member.is_function())
}

/// The `AudioContext` constructor, prefixed where it must be.
#[must_use]
pub fn audio_context_constructor() -> Option<Function> {
    let window = window()?;
    for name in ["AudioContext", "webkitAudioContext"] {
        let value = Reflect::get(window.as_ref(), &JsValue::from_str(name)).ok()?;
        if let Ok(function) = value.dyn_into::<Function>() {
            return Some(function);
        }
    }
    None
}

/// Ask the Permissions API once for the whole page.
///
/// The answer arrives as a promise with no component to re-render, so it latches
/// into the page-level flag `mic_permission` reads. A browser that refuses the
/// query leaves the latch alone, which is the same answer as `prompt`.
pub fn probe_mic_permission() {
    let Some(permissions) = permissions() else {
        return;
    };
    let Ok(promise) = permissions.query(&microphone_permission_descriptor()) else {
        return;
    };
    let answered = Closure::wrap(Box::new(|value: JsValue| {
        if let Ok(status) = value.dyn_into::<web_sys::PermissionStatus>()
            && status.state() == PermissionState::Granted
        {
            note_mic_granted();
        }
    }) as Box<dyn FnMut(JsValue)>);
    // A rejected query is a browser without the permission name, which the
    // engine reads as "unknown" rather than as "denied".
    let refused = Closure::wrap(Box::new(|_error: JsValue| {}) as Box<dyn FnMut(JsValue)>);
    let _ = promise.then(&answered).catch(&refused);
    answered.forget();
    refused.forget();
}

fn permissions() -> Option<Permissions> {
    let navigator = window()?.navigator();
    Reflect::get(navigator.as_ref(), &JsValue::from_str("permissions"))
        .ok()?
        .dyn_into::<Permissions>()
        .ok()
}

/// The `{ name: "microphone" }` descriptor the query takes.
fn microphone_permission_descriptor() -> Object {
    let descriptor = Object::new();
    let _ = Reflect::set(
        descriptor.as_ref(),
        &JsValue::from_str("name"),
        &JsValue::from_str("microphone"),
    );
    descriptor
}
