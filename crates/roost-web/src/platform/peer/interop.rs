//! How this file talks to the browser's own objects: `Reflect` for property
//! writes, `Function::apply` for every call, and the three transport faults
//! those calls turn into.
//!
//! A file split, not a type split. `peer` owns WHAT the carrier does — open a
//! peer, answer it, write a lane, probe it — and this owns the seven browser
//! primitives it does it with. Every call is checked and every refusal is
//! returned, because a throw inside a handler is a fact the caller has to
//! settle rather than an exception it can catch.

#[cfg(target_arch = "wasm32")]
use {
    js_sys::Reflect,
    wasm_bindgen::{JsCast as _, JsValue},
};

use roost_client_core::client::carriers::TransportError;

/// Open one static, ordered, negotiated data channel.
///
/// `negotiated: true` with an explicit `id` is what makes the channel exist
/// before either end has exchanged an SDP — which is what lets the browser write
/// the carrier's `hello` on the control lane as soon as the transport opens,
/// instead of after a second round trip through the coordinator.
#[cfg(target_arch = "wasm32")]
pub(super) fn open_channel(
    connection: &JsValue,
    label: &str,
    stream_id: u16,
) -> Result<JsValue, TransportError> {
    use js_sys::{Array, Object};

    let init = Object::new();
    for (key, value) in [
        ("id", JsValue::from_f64(f64::from(stream_id))),
        ("negotiated", JsValue::TRUE),
        ("ordered", JsValue::TRUE),
    ] {
        set_prop(&init, key, &value)
            .map_err(|_| refused(&format!("the {label} options were refused")))?;
    }
    call_method(
        connection,
        "createDataChannel",
        &Array::of2(&JsValue::from_str(label), &init.into()),
    )
}

/// A session description object. A fresh plain object never refuses a property,
/// so the two writes carry no error path worth threading: a refusal would mean
/// `Object::new` itself misbehaved.
#[cfg(target_arch = "wasm32")]
pub(super) fn description_object(kind: &str, sdp: &str) -> JsValue {
    use js_sys::Object;

    let description = Object::new();
    let _ = Reflect::set(
        &description,
        &JsValue::from_str("type"),
        &JsValue::from_str(kind),
    );
    let _ = Reflect::set(
        &description,
        &JsValue::from_str("sdp"),
        &JsValue::from_str(sdp),
    );
    description.into()
}

/// The one place the browser's objects are invoked, and every call is checked:
/// a throw is a fact the caller settles, not an exception mid-handler.
#[cfg(target_arch = "wasm32")]
pub(super) fn call_method(
    object: &JsValue,
    name: &str,
    args: &js_sys::Array,
) -> Result<JsValue, TransportError> {
    use js_sys::Function;

    let method = Reflect::get(object, &JsValue::from_str(name))
        .map_err(|_| refused(&format!("{name} could not be read")))?
        .dyn_into::<Function>()
        .map_err(|_| refused(&format!("{name} is not callable")))?;
    method
        .apply(object, args)
        .map_err(|_| refused(&format!("{name} was refused by the browser")))
}

#[cfg(target_arch = "wasm32")]
pub(super) fn set_prop(
    object: &js_sys::Object,
    key: &str,
    value: &JsValue,
) -> Result<(), TransportError> {
    // `Reflect::set` answers whether the property was set, which is not what
    // the caller asked: it asked whether the set was REFUSED. The refusal is the
    // error arm, and the boolean is dropped deliberately.
    Reflect::set(object, &JsValue::from_str(key), value)
        .map(|_| ())
        .map_err(|_| refused(&format!("{key} could not be set")))
}

pub(super) fn unavailable(detail: &str) -> TransportError {
    TransportError::Unavailable {
        detail: detail.to_string(),
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn refused(detail: &str) -> TransportError {
    TransportError::Refused {
        detail: detail.to_string(),
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn closed() -> TransportError {
    TransportError::Closed {
        reason: "no open peer for this attempt".to_string(),
    }
}
