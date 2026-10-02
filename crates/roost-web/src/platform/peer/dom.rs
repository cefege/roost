//! The browser objects this document's peers are made of, and the callbacks
//! that report what they did.
//!
//! Owned by `platform::peer`. A file split, not a type split: `peer` owns WHAT
//! a carrier does — open a peer, read its offer, take an answer, write a lane,
//! close it — and this owns the `RTCPeerConnection`, the three data channels,
//! and the handlers that turn browser callbacks into [`PeerEvent`]s. Every
//! callback pushes into a sink and returns; none of them reaches for the store,
//! because a data channel can fire while the pump is already borrowed.
//!
//! `wasm32` only, and every handler is installed as a `Closure` the caller keeps
//! alive: dropping the closure drops the browser's reference to it, and a
//! handler that fires into a freed closure is a peer that faults with no cause.

#[cfg(target_arch = "wasm32")]
use std::collections::BTreeMap;

#[cfg(target_arch = "wasm32")]
use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::carriers::PeerLane;

#[cfg(target_arch = "wasm32")]
use super::events::{PeerEvent, PeerEventSink};

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::carriers::TransportError;

#[cfg(target_arch = "wasm32")]
use super::interop::{call_method, description_object, open_channel, set_prop};

/// `globalThis.RTCPeerConnection`, named once so nothing else spells the
/// constructor. This is the only WebRTC type in the tree.
#[cfg(target_arch = "wasm32")]
pub(super) const RTC_PEER_CONNECTION: &str = "RTCPeerConnection";

/// One open peer: its connection and its three negotiated channels.
#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub(super) struct BrowserPeer {
    pub(super) connection: JsValue,
    pub(super) channels: BTreeMap<u16, JsValue>,
}

/// The handlers installed on one peer, held as long as the peer is.
///
/// A browser holds a raw reference to each one, so this list is what keeps a
/// handler callable: dropping it is not a leak being fixed, it is the handler
/// becoming a dangling call. Type-erased because the message handler takes the
/// browser's event object and the other five do not, and a list is a list.
#[cfg(target_arch = "wasm32")]
pub(super) type Handlers = Vec<Box<dyn std::any::Any>>;

/// Construct a peer connection with the coordinator's discovery configuration.
///
/// STUN is passed through as the grant named it, empty included: an empty list
/// is the operator saying "no external discovery", and replacing it with a
/// default would reach the internet on their behalf.
#[cfg(target_arch = "wasm32")]
pub(super) fn construct(stun_urls: &[String]) -> Result<JsValue, TransportError> {
    use super::interop::unavailable;

    let constructor = Reflect::get(&js_sys::global(), &JsValue::from_str(RTC_PEER_CONNECTION))
        .ok()
        .and_then(|value| value.dyn_into::<Function>().ok())
        .ok_or_else(|| unavailable("this document cannot construct a peer"))?;

    let configuration = Object::new();
    if !stun_urls.is_empty() {
        let servers = Array::new();
        for url in stun_urls {
            let server = Object::new();
            set_prop(&server, "urls", &JsValue::from_str(url))
                .map_err(|_| unavailable("a STUN server could not be built"))?;
            servers.push(&server);
        }
        set_prop(&configuration, "iceServers", &servers)
            .map_err(|_| unavailable("the peer configuration could not be built"))?;
    }

    // `constructor` is the `RTCPeerConnection` FUNCTION, not a Rust
    // constructor, so `new_with` is the wrong call. Constructing through
    // `Reflect` is what invokes a JS class with an argument list, and it
    // reports the browser's refusal the way every other call here does.
    Reflect::construct(&constructor, &Array::of1(&configuration))
        .map_err(|_| unavailable("the browser refused an RTCPeerConnection"))
}

/// Open the three static, ordered, negotiated channels, in protocol order.
///
/// In band, because `negotiated: true` with an explicit id is what makes a
/// channel exist before either end has exchanged an SDP — which is what lets the
/// browser spend its credential on the control lane as soon as the transport
/// opens rather than after a second round trip.
#[cfg(target_arch = "wasm32")]
pub(super) fn open_lanes(
    connection: &JsValue,
    lanes: &[(PeerLane, &'static str)],
) -> Result<BTreeMap<u16, JsValue>, TransportError> {
    use super::interop::unavailable;

    let mut channels = BTreeMap::new();
    for (lane, label) in lanes {
        let channel = open_channel(connection, label, lane.stream_id())
            .map_err(|_| unavailable(&format!("the {label} channel was refused")))?;
        // `arraybuffer` is what makes `event.data` an `ArrayBuffer` rather than
        // a `Blob`: a `Blob` cannot be read synchronously, and a carrier that
        // has to await its own frame reorders against the lane's ordering.
        let _ = Reflect::set(
            &channel,
            &JsValue::from_str("binaryType"),
            &JsValue::from_str("arraybuffer"),
        );
        channels.insert(lane.stream_id(), channel);
    }
    Ok(channels)
}

/// Create the local offer and hand it to the connection, which starts ICE.
///
/// Both calls are PROMISES (v2: `await setLocalDescription(await createOffer())`),
/// so they settle on a task and report through the sink. A browser that refuses
/// either has no offer to give, and that is reported as the attempt's end rather
/// than read later as an empty description.
#[cfg(target_arch = "wasm32")]
pub(super) fn begin_gathering(
    connection: &JsValue,
    attempt_id: u64,
    sink: &PeerEventSink,
) -> Result<(), TransportError> {
    let offer = as_promise(call_method(connection, "createOffer", &Array::new())?)?;
    let connection = connection.clone();
    let sink = sink.clone();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(reason) = describe_locally(&connection, offer).await {
            sink.record(PeerEvent::IceFailed { attempt_id, reason });
            return;
        }
        // A host-only gatherer can finish before this task resumes; the state
        // handler saw that transition too, and the offer is read once either way.
        if gathering_complete(&connection) {
            sink.record(PeerEvent::Gathered { attempt_id });
        }
    });
    Ok(())
}

/// Hand the coordinator's answer to the connection, and report a refusal.
///
/// v2 awaits `setRemoteDescription` and closes the peer when it rejects; the
/// rejection is reported here as the attempt's end for the same reason.
#[cfg(target_arch = "wasm32")]
pub(super) fn apply_remote_answer(
    connection: &JsValue,
    answer_sdp: &str,
    attempt_id: u64,
    sink: &PeerEventSink,
) -> Result<(), TransportError> {
    let applied = as_promise(call_method(
        connection,
        "setRemoteDescription",
        &Array::of1(&description_object("answer", answer_sdp)),
    )?)?;
    let sink = sink.clone();
    wasm_bindgen_futures::spawn_local(async move {
        if JsFuture::from(applied).await.is_err() {
            sink.record(PeerEvent::IceFailed {
                attempt_id,
                reason: "terminal peer setRemoteDescription failed".to_owned(),
            });
        }
    });
    Ok(())
}

/// `createOffer` resolved, then `setLocalDescription` accepted what it produced.
#[cfg(target_arch = "wasm32")]
async fn describe_locally(connection: &JsValue, offer: Promise) -> Result<(), String> {
    let description = JsFuture::from(offer)
        .await
        .map_err(|_| "terminal peer createOffer failed".to_owned())?;
    let applied = call_method(connection, "setLocalDescription", &Array::of1(&description))
        .and_then(as_promise)
        .map_err(|error| error.to_string())?;
    JsFuture::from(applied)
        .await
        .map(|_| ())
        .map_err(|_| "terminal peer setLocalDescription failed".to_owned())
}

/// A browser method's return value, checked to be the promise it is specified as.
#[cfg(target_arch = "wasm32")]
fn as_promise(value: JsValue) -> Result<Promise, TransportError> {
    value
        .dyn_into::<Promise>()
        .map_err(|_| super::interop::refused("the browser returned no promise"))
}

/// The SDP the browser has gathered so far, unfiltered.
#[cfg(target_arch = "wasm32")]
pub(super) fn gathered_sdp(connection: &JsValue) -> Result<String, TransportError> {
    use super::interop::refused;

    let description = Reflect::get(connection, &JsValue::from_str("localDescription"))
        .map_err(|_| refused("the peer has no local description"))?;
    Reflect::get(&description, &JsValue::from_str("sdp"))
        .ok()
        .and_then(|value| value.as_string())
        .ok_or_else(|| refused("the local description has no SDP yet"))
}

/// Whether this connection's ICE gathering has finished.
#[cfg(target_arch = "wasm32")]
pub(super) fn gathering_complete(connection: &JsValue) -> bool {
    Reflect::get(connection, &JsValue::from_str("iceGatheringState"))
        .ok()
        .and_then(|value| value.as_string())
        .is_some_and(|state| state == "complete")
}

/// The one ICE state that means "no connection will ever come up here".
#[cfg(target_arch = "wasm32")]
pub(super) fn ice_failed(connection: &JsValue) -> Option<String> {
    let state = Reflect::get(connection, &JsValue::from_str("iceConnectionState"))
        .ok()
        .and_then(|value| value.as_string())?;
    matches!(state.as_str(), "failed" | "closed").then(|| format!("terminal peer ICE {state}"))
}

/// Write bytes on one channel.
#[cfg(target_arch = "wasm32")]
pub(super) fn write(channel: &JsValue, bytes: &[u8]) -> Result<(), TransportError> {
    let payload = Uint8Array::from(bytes);
    call_method(channel, "send", &Array::of1(&payload.into())).map(|_| ())
}

/// Close every channel on a peer, then the connection.
///
/// A refusal from the browser is ignored: the peer is already gone from the
/// table either way, and a close that reported its own teardown as a failure
/// would send the negotiation down a fault path for a carrier that is gone.
#[cfg(target_arch = "wasm32")]
pub(super) fn close(peer: &BrowserPeer) {
    for channel in peer.channels.values() {
        let _ = call_method(channel, "close", &Array::new());
    }
    let _ = call_method(&peer.connection, "close", &Array::new());
}

/// Install the connection's two ICE handlers.
#[cfg(target_arch = "wasm32")]
pub(super) fn install_connection_handlers(
    connection: &JsValue,
    attempt_id: u64,
    sink: &PeerEventSink,
) -> Handlers {
    let gathering = Closure::<dyn FnMut()>::new({
        let connection = connection.clone();
        let sink = sink.clone();
        move || {
            if gathering_complete(&connection) {
                sink.record(PeerEvent::Gathered { attempt_id });
            }
        }
    });
    let ice = Closure::<dyn FnMut()>::new({
        let connection = connection.clone();
        let sink = sink.clone();
        move || {
            if let Some(reason) = ice_failed(&connection) {
                sink.record(PeerEvent::IceFailed { attempt_id, reason });
            }
        }
    });
    assign(connection, "onicegatheringstatechange", &gathering);
    assign(connection, "oniceconnectionstatechange", &ice);
    vec![Box::new(gathering), Box::new(ice)]
}

/// Install a channel's open, close and error handlers.
#[cfg(target_arch = "wasm32")]
pub(super) fn install_channel_handlers(
    channel: &JsValue,
    lane: PeerLane,
    attempt_id: u64,
    sink: &PeerEventSink,
) -> Handlers {
    let open = Closure::<dyn FnMut()>::new({
        let sink = sink.clone();
        move || {
            sink.record(PeerEvent::LaneOpen { attempt_id, lane });
        }
    });
    let closed = Closure::<dyn FnMut()>::new({
        let sink = sink.clone();
        move || {
            sink.record(PeerEvent::LaneFailed {
                attempt_id,
                lane,
                reason: format!("terminal peer {} lane closed", lane.label()),
            });
        }
    });
    let failed = Closure::<dyn FnMut()>::new({
        let sink = sink.clone();
        move || {
            sink.record(PeerEvent::LaneFailed {
                attempt_id,
                lane,
                reason: format!("terminal peer {} lane failed", lane.label()),
            });
        }
    });
    assign(channel, "onopen", &open);
    assign(channel, "onclose", &closed);
    assign(channel, "onerror", &failed);
    vec![Box::new(open), Box::new(closed), Box::new(failed)]
}

/// Attach the per-message reader, the one handler that needs the event object.
#[cfg(target_arch = "wasm32")]
pub(super) fn install_message_handler(
    channel: &JsValue,
    lane: PeerLane,
    attempt_id: u64,
    sink: &PeerEventSink,
) -> Handlers {
    let sink = sink.clone();
    // The browser calls `onmessage` with ONE argument, the `MessageEvent`
    // itself; reading it as an argument list finds no element 0 and drops every
    // message, which is a worker whose `Ready` never arrives.
    let handler = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
        let Ok(data) = Reflect::get(&event, &JsValue::from_str("data")) else {
            return;
        };
        // `binaryType` was set to `arraybuffer` when the channel opened, so
        // `data` is an `ArrayBuffer`; a non-object here means the browser gave
        // something else, and a `Blob` cannot be read synchronously without
        // reordering against the lane's ordering guarantee.
        if !data.is_object() {
            return;
        }
        let bytes = Uint8Array::new(&data).to_vec();
        if !bytes.is_empty() {
            sink.record(PeerEvent::Bytes {
                attempt_id,
                lane,
                bytes,
            });
        }
    });
    assign(channel, "onmessage", &handler);
    vec![Box::new(handler)]
}

/// Write one handler onto a browser object.
///
/// `Reflect::set` rather than a field write because these are JS-defined
/// properties on objects this crate holds no static type for, and a refusal
/// means the browser will not call back at all — which is why it is a warning
/// naming the handler rather than a silent no-op.
#[cfg(target_arch = "wasm32")]
fn assign<T: ?Sized>(target: &JsValue, name: &str, handler: &Closure<T>) {
    let value: &JsValue = handler.as_ref();
    let function = value.unchecked_ref::<js_sys::Function>();
    if Reflect::set(target, &JsValue::from_str(name), function).is_err() {
        tracing::warn!(target: "peer", handler = name, "the browser refused this peer handler");
    }
}
