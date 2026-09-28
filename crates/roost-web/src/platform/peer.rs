//! The browser's WebRTC stack, as one `PeerTransport`, and the only file in this
//! tree that names a WebRTC binding. Owned by `platform`, driven by the client
//! core's `PeerSignalling` machine. Ported from
//! `apps/web/src/store/transport/terminal-peer-connection.ts`: the three static
//! ordered channels and the offer/answer handshake, and nothing else.
//!
//! The bindings are reached through `js_sys::Reflect` rather than `web_sys`
//! because this crate's `web-sys` feature list carries no WebRTC features — no
//! `RtcPeerConnection`, no `RtcDataChannel`, no `RtcConfiguration`. That is a
//! statement about the FEATURE SET, not about reachability: the module is
//! compiled because `platform` declares it, and it is reachable like any other.
//! Because it names no `web_sys` type, it builds against the features the crate
//! actually declares; the wasm32 build is what type-checks the browser-only
//! items below. The open-peer bookkeeping is `peer/table.rs`, target-independent
//! and natively tested, so no counting rule lives behind the `wasm32` gate.

#[cfg(target_arch = "wasm32")]
use std::collections::BTreeMap;

use roost_client_core::client::carriers::{PeerAttempt, PeerLane, PeerTransport, TransportError};

pub use roost_client_core::client::carriers::{
    CHANNEL_TERMINAL_CONTROL_V1, CHANNEL_TERMINAL_DATA_V1, CHANNEL_TERMINAL_HISTORY_V1,
};

mod interop;
mod table;

pub use table::PeerTable;

// Six of the seven helpers are browser-only, exactly as they were here, so the
// import is gated the same way they are. `unavailable` is not: the native arm's
// `no_browser` returns it, which is why it lives in the moved group at all.
use interop::unavailable;
#[cfg(target_arch = "wasm32")]
use interop::{call_method, closed, description_object, open_channel, refused, set_prop};

#[cfg(target_arch = "wasm32")]
use {
    js_sys::Reflect,
    wasm_bindgen::{JsCast as _, JsValue},
};

/// `globalThis.RTCPeerConnection`, named once so nothing else spells the
/// constructor. This is the only WebRTC type in the tree.
#[cfg(target_arch = "wasm32")]
const RTC_PEER_CONNECTION: &str = "RTCPeerConnection";

/// The three lanes, in the order the protocol numbers them. The order IS the
/// stream id: two ends that number their channels differently do not pair them.
#[cfg(target_arch = "wasm32")]
const LANES: [(PeerLane, &str); 3] = [
    (PeerLane::Control, CHANNEL_TERMINAL_CONTROL_V1),
    (PeerLane::Data, CHANNEL_TERMINAL_DATA_V1),
    (PeerLane::History, CHANNEL_TERMINAL_HISTORY_V1),
];

/// Whether this build has a browser WebRTC stack at all.
///
/// False everywhere except a `wasm32` build, and false there too unless the
/// document is a secure context exposing the constructor. A caller asks this
/// BEFORE requesting a grant, so a document that cannot peer never makes a
/// worker install a peer credential for it.
pub fn is_available() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        Reflect::get(&js_sys::global(), &JsValue::from_str(RTC_PEER_CONNECTION))
            .ok()
            .filter(|constructor| constructor.is_function())
            .is_some()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // No document and no `RTCPeerConnection` here, so the honest answer is
        // no rather than a stack that would only fail at runtime.
        false
    }
}

/// One open peer and its three negotiated channels.
///
/// Owned by the adapter, not the core: the core's token is a generation, and a
/// transport is a live object with callbacks attached. Keyed by attempt id in
/// [`PeerTable`], so an event naming a dead attempt is discarded rather than
/// applied to whatever connection took its place.
#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
struct OpenPeer {
    connection: JsValue,
    channels: BTreeMap<u16, JsValue>,
}

/// A build with no browser never opens a peer, so there is nothing to hold.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
enum OpenPeer {}

/// The browser's WebRTC stack, behind the client core's own transport trait.
/// Total on every target: a build with no browser answers every open the one
/// way that is true, which is `Unavailable`, and so never holds a peer.
#[derive(Debug, Default)]
pub struct BrowserPeer {
    peers: PeerTable<OpenPeer>,
}

impl BrowserPeer {
    /// A fresh adapter. It holds nothing until an `open` succeeds.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many peers this adapter is holding, for the document-wide cap.
    pub fn open_count(&self) -> usize {
        self.peers.open_count()
    }
}

#[cfg(target_arch = "wasm32")]
impl PeerTransport for BrowserPeer {
    /// Open the three ordered channels and start ICE gathering.
    ///
    /// This returns nothing to send, because there is nothing to send YET: a
    /// browser fills the local description in as candidates gather, and the
    /// gathering deadline that decides when to read it belongs to the core. The
    /// host calls `local_offer` once that deadline has passed.
    fn open(&mut self, attempt: &PeerAttempt) -> Result<(), TransportError> {
        use js_sys::{Array, Function, Object};

        let constructor = Reflect::get(&js_sys::global(), &JsValue::from_str(RTC_PEER_CONNECTION))
            .ok()
            .and_then(|value| value.dyn_into::<Function>().ok())
            .ok_or_else(|| unavailable("this document cannot construct a peer"))?;

        // STUN is opportunistic address discovery. The coordinator already
        // parsed and bounded the URLs it sent, and an empty list is the operator
        // saying "no external discovery" — so it is passed through as empty
        // rather than replaced with a default that would reach the internet on
        // their behalf.
        let configuration = Object::new();
        if !attempt.stun_urls.is_empty() {
            let servers = Array::new();
            for url in &attempt.stun_urls {
                let server = Object::new();
                set_prop(&server, "urls", &JsValue::from_str(url))
                    .map_err(|_| unavailable("a STUN server could not be built"))?;
                servers.push(&server);
            }
            set_prop(&configuration, "iceServers", &servers)
                .map_err(|_| unavailable("the peer configuration could not be built"))?;
        }

        // `constructor` is the `RTCPeerConnection` FUNCTION, not a Rust
        // constructor, so `new_with` (which builds a function) is the wrong
        // call. Constructing through `Reflect` is what invokes a JS class with
        // an argument list, and it reports the browser's refusal the same way
        // every other call in this file does.
        let connection = Reflect::construct(&constructor, &Array::of1(&configuration))
            .map_err(|_| unavailable("the browser refused an RTCPeerConnection"))?;

        let mut channels = BTreeMap::new();
        for (lane, label) in LANES {
            let channel = open_channel(&connection, label, lane.stream_id())
                .map_err(|_| unavailable(&format!("the {label} channel was refused")))?;
            channels.insert(lane.stream_id(), channel);
        }

        let offer = call_method(&connection, "createOffer", &Array::new())?;
        let sdp = Reflect::get(&offer, &JsValue::from_str("sdp"))
            .ok()
            .and_then(|value| value.as_string())
            .ok_or_else(|| refused("the browser produced no local description"))?;
        call_method(
            &connection,
            "setLocalDescription",
            &Array::of1(&description_object("offer", &sdp)),
        )?;

        let opened = OpenPeer {
            connection,
            channels,
        };
        // An attempt id is never reused by the core; if one were, the peer it
        // displaced is closed rather than left open behind the new one.
        if let Some(displaced) = self.peers.open(attempt.attempt_id, opened) {
            close_browser_peer(&displaced);
        }
        Ok(())
    }

    /// Read the local description, with the browser's own candidates filtered
    /// down to the ones a worker may be told about.
    ///
    /// The filter is not an optimization: a host or mDNS candidate is an address
    /// disclosure, and the coordinator relays the offer to a worker on another
    /// machine (`protocol/spec/direct-terminal.md:26`).
    fn local_offer(&self, attempt_id: u64) -> Result<String, TransportError> {
        let connection = self.connection_of(attempt_id)?;
        let description = Reflect::get(&connection, &JsValue::from_str("localDescription"))
            .map_err(|_| refused("the peer has no local description"))?;
        let sdp = Reflect::get(&description, &JsValue::from_str("sdp"))
            .ok()
            .and_then(|value| value.as_string())
            .ok_or_else(|| refused("the local description has no SDP yet"))?;
        Ok(roost_protocol::terminal_peer::sdp::filter_browser_terminal_peer_udp_candidates(&sdp))
    }

    /// Hand the coordinator's answer to the open transport.
    fn accept_answer(&mut self, attempt_id: u64, answer_sdp: &str) -> Result<(), TransportError> {
        use js_sys::Array;
        let connection = self.connection_of(attempt_id)?;
        call_method(
            &connection,
            "setRemoteDescription",
            &Array::of1(&description_object("answer", answer_sdp)),
        )
        .map(|_| ())
    }

    /// Write one already-framed packet on one lane. The framing is the CALLER's:
    /// a transport moves bytes and knows nothing about the carrier's encoding,
    /// which is what keeps the packet header out of every WebRTC stack.
    fn send(
        &mut self,
        attempt_id: u64,
        lane: PeerLane,
        bytes: &[u8],
    ) -> Result<(), TransportError> {
        use js_sys::{Array, Uint8Array};
        let channel = self.channel_of(attempt_id, lane)?;
        let payload = Uint8Array::from(bytes);
        call_method(&channel, "send", &Array::of1(&payload.into())).map(|_| ())
    }

    /// Write one already-framed content-free probe. Two unanswered probes retire
    /// the peer, and the count is the caller's because the reply it correlates is
    /// the caller's too.
    fn probe(&mut self, attempt_id: u64, frame: &[u8]) -> Result<(), TransportError> {
        self.send(attempt_id, PeerLane::Control, frame)
    }

    /// Close the peer and every channel on it. Closing one that is already gone
    /// is not an error: a fault path and a page teardown both reach here.
    fn close(&mut self, attempt_id: u64, _reason: &str) {
        if let Some(peer) = self.peers.close(attempt_id) {
            close_browser_peer(&peer);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl PeerTransport for BrowserPeer {
    fn open(&mut self, _attempt: &PeerAttempt) -> Result<(), TransportError> {
        Err(no_browser())
    }

    fn local_offer(&self, _attempt_id: u64) -> Result<String, TransportError> {
        Err(no_browser())
    }

    fn accept_answer(&mut self, _attempt_id: u64, _answer_sdp: &str) -> Result<(), TransportError> {
        Err(no_browser())
    }

    fn send(&mut self, _id: u64, _lane: PeerLane, _bytes: &[u8]) -> Result<(), TransportError> {
        Err(no_browser())
    }

    fn probe(&mut self, _attempt_id: u64, _frame: &[u8]) -> Result<(), TransportError> {
        Err(no_browser())
    }

    fn close(&mut self, _attempt_id: u64, _reason: &str) {}
}

#[cfg(not(target_arch = "wasm32"))]
fn no_browser() -> TransportError {
    unavailable("this build has no browser WebRTC stack")
}

/// Close every channel on a peer, then the connection. A refusal from the
/// browser is ignored: the peer is already gone from the table either way.
#[cfg(target_arch = "wasm32")]
fn close_browser_peer(peer: &OpenPeer) {
    use js_sys::Array;
    for channel in peer.channels.values() {
        let _ = call_method(channel, "close", &Array::new());
    }
    let _ = call_method(&peer.connection, "close", &Array::new());
}

#[cfg(target_arch = "wasm32")]
impl BrowserPeer {
    fn connection_of(&self, attempt_id: u64) -> Result<JsValue, TransportError> {
        self.peers
            .get(attempt_id)
            .map(|peer| peer.connection.clone())
            .ok_or_else(closed)
    }

    fn channel_of(&self, attempt_id: u64, lane: PeerLane) -> Result<JsValue, TransportError> {
        let peer = self.peers.get(attempt_id).ok_or_else(closed)?;
        peer.channels
            .get(&lane.stream_id())
            .cloned()
            .ok_or_else(|| refused(&format!("this peer has no {} lane", lane.label())))
    }
}
