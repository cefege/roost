//! The browser objects one attachment peer is made of, and the callbacks that
//! turn what they report into [`PeerEvent`]s.
//!
//! Owned by `platform::attachments`; `super::peer` builds a carrier from these.
//! A file split, not a type split: `peer` decides what an event means, and this
//! only constructs the connection and pushes what the browser observed.

use js_sys::{Array, Object, Reflect, Uint8Array};
use roost_client_core::client::attachments::packets::PeerLane;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};
use web_sys::{MessageEvent, RtcConfiguration, RtcDataChannel, RtcPeerConnection};

use super::inbox::EventInbox;

/// One thing the peer's callbacks observed.
#[derive(Debug)]
pub enum PeerEvent {
    ChannelOpen(PeerLane),
    Packet(PeerLane, Vec<u8>),
    BufferedLow,
    GatheringComplete,
    /// A lifecycle event that ends the carrier, with v2's reason for it.
    Ended(&'static str),
}

/// The callbacks a peer's objects hold, kept alive for as long as they fire.
pub type PeerListeners = Vec<Closure<dyn FnMut(JsValue)>>;

/// A peer configured with the grant's STUN list, empty included: an empty list
/// is the operator saying "no external discovery".
pub fn construct(stun_urls: &[String]) -> Option<RtcPeerConnection> {
    let configuration = Object::new();
    let servers = Array::new();
    for url in stun_urls {
        let server = Object::new();
        Reflect::set(&server, &"urls".into(), &JsValue::from_str(url)).ok()?;
        servers.push(&server);
    }
    Reflect::set(&configuration, &"iceServers".into(), &servers).ok()?;
    Reflect::set(&configuration, &"iceTransportPolicy".into(), &"all".into()).ok()?;
    RtcPeerConnection::new_with_configuration(&configuration.unchecked_into::<RtcConfiguration>())
        .ok()
}

/// Route the connection's state, gathering and unsolicited-offer callbacks.
pub fn install_connection_listeners(
    connection: &RtcPeerConnection,
    inbox: &EventInbox<PeerEvent>,
) -> PeerListeners {
    let watched = connection.clone();
    let on_state = listener(inbox, move |_| {
        let failed = |name| {
            matches!(
                string_prop(&watched, name).as_deref(),
                Some("failed" | "closed")
            )
        };
        if failed("connectionState") {
            Some(PeerEvent::Ended("attachment peer connection failed"))
        } else if failed("iceConnectionState") {
            Some(PeerEvent::Ended("attachment peer ICE failed"))
        } else {
            None
        }
    });
    let gathering = connection.clone();
    let on_gathering = listener(inbox, move |_| {
        (string_prop(&gathering, "iceGatheringState").as_deref() == Some("complete"))
            .then_some(PeerEvent::GatheringComplete)
    });
    let on_channel = listener(inbox, |_| {
        Some(PeerEvent::Ended(
            "attachment peer offered an unsolicited data channel",
        ))
    });
    let on_track = listener(inbox, |_| {
        Some(PeerEvent::Ended("attachment peer offered a media track"))
    });
    connection.set_onconnectionstatechange(Some(on_state.as_ref().unchecked_ref()));
    connection.set_oniceconnectionstatechange(Some(on_state.as_ref().unchecked_ref()));
    connection.set_onicegatheringstatechange(Some(on_gathering.as_ref().unchecked_ref()));
    connection.set_ondatachannel(Some(on_channel.as_ref().unchecked_ref()));
    connection.set_ontrack(Some(on_track.as_ref().unchecked_ref()));
    vec![on_state, on_gathering, on_channel, on_track]
}

/// Route one lane's channel callbacks.
pub fn install_channel_listeners(
    channel: &RtcDataChannel,
    lane: PeerLane,
    inbox: &EventInbox<PeerEvent>,
) -> PeerListeners {
    let on_open = listener(inbox, move |_| Some(PeerEvent::ChannelOpen(lane)));
    let on_close = listener(inbox, |_| {
        Some(PeerEvent::Ended("attachment peer channel closed"))
    });
    let on_error = listener(inbox, |_| {
        Some(PeerEvent::Ended("attachment peer channel failed"))
    });
    let on_low = listener(inbox, |_| Some(PeerEvent::BufferedLow));
    let on_message = listener(inbox, move |event| {
        let data = event.unchecked_into::<MessageEvent>().data();
        Some(match data.dyn_into::<js_sys::ArrayBuffer>() {
            Ok(buffer) => PeerEvent::Packet(lane, Uint8Array::new(&buffer).to_vec()),
            Err(_) => PeerEvent::Ended("attachment peer received an invalid packet"),
        })
    });
    channel.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    channel.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    channel.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    channel.set_onbufferedamountlow(Some(on_low.as_ref().unchecked_ref()));
    channel.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    vec![on_open, on_close, on_error, on_low, on_message]
}

/// A string property the typed bindings gate behind features this crate does
/// not enable.
pub fn string_prop(target: &JsValue, name: &str) -> Option<String> {
    Reflect::get(target, &JsValue::from_str(name))
        .ok()
        .and_then(|value| value.as_string())
}

/// A callback that pushes what `observe` makes of its event, if anything.
fn listener(
    inbox: &EventInbox<PeerEvent>,
    observe: impl Fn(JsValue) -> Option<PeerEvent> + 'static,
) -> Closure<dyn FnMut(JsValue)> {
    let inbox = inbox.clone();
    Closure::wrap(Box::new(move |event: JsValue| {
        if let Some(observed) = observe(event) {
            inbox.push(observed);
        }
    }) as Box<dyn FnMut(JsValue)>)
}
