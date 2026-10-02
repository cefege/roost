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
//!
//! What this owns is the TRANSPORT: open a peer, gather an offer, take an
//! answer, move bytes, close. What the offer is SPENT on — the coordinator
//! round trip, the credential, the handshake and the election — is
//! `pump::peer_dial`'s, and what the bytes mean is `client::carriers::wire`'s. A
//! browser callback here records a fact and returns; it never reaches for the
//! store, because a data channel can fire while the pump is already borrowed.

use roost_client_core::client::carriers::{PeerAttempt, PeerLane, PeerTransport, TransportError};

#[cfg(not(target_arch = "wasm32"))]
pub use availability::force_availability_for_test;
pub use availability::is_available;
pub use events::{Overflowed, PEER_EVENT_BACKLOG, PeerEvent, PeerEventSink, PeerMeasurement};
pub use roost_client_core::client::carriers::{
    CHANNEL_TERMINAL_CONTROL_V1, CHANNEL_TERMINAL_DATA_V1, CHANNEL_TERMINAL_HISTORY_V1,
};

mod availability;
#[cfg(target_arch = "wasm32")]
mod dom;
mod events;
mod interop;
#[cfg(target_arch = "wasm32")]
mod stats;
mod table;

pub use table::PeerTable;

// Six of the seven helpers are browser-only, exactly as they were here, so the
// import is gated the same way they are. `unavailable` is not browser-only: the
// native arm's `no_browser` returns it.
#[cfg(not(target_arch = "wasm32"))]
use interop::unavailable;

/// The three lanes, in the order the protocol numbers them. The order IS the
/// stream id: two ends that number their channels differently do not pair them.
#[cfg(target_arch = "wasm32")]
const LANES: [(PeerLane, &'static str); 3] = [
    (PeerLane::Control, CHANNEL_TERMINAL_CONTROL_V1),
    (PeerLane::Data, CHANNEL_TERMINAL_DATA_V1),
    (PeerLane::History, CHANNEL_TERMINAL_HISTORY_V1),
];

/// One open peer: its browser objects, its handlers, and what it was opened for.
///
/// The ATTEMPT is held here rather than re-derived: the negotiation's later
/// steps name only an attempt id, and the coordinator round trip needs the grant
/// id, the peer id and the worker epoch the attempt already carries. Keyed by
/// attempt id in [`PeerTable`], so an event naming a dead attempt is discarded
/// rather than applied to whatever connection took its place.
#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
struct OpenPeer {
    browser: dom::BrowserPeer,
    /// Held as long as the peer is and never read: dropping it drops the
    /// browser's references to every callback the peer reports through.
    _handlers: dom::Handlers,
    attempt: PeerAttempt,
    /// Which lanes the browser has reported open, by stream id.
    open_lanes: [bool; 3],
}

/// A build with no browser never opens a peer, so nothing is ever held in it. The
/// fields the target-independent accessors read exist anyway, so those accessors
/// keep ONE answer on this target too — and the answer is "no", which is the
/// truth: there is no connection to ask.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
struct OpenPeer {
    attempt: PeerAttempt,
    open_lanes: [bool; 3],
}

/// The browser's WebRTC stack, behind the client core's own transport trait.
/// Total on every target: a build with no browser answers every open the one
/// way that is true, which is `Unavailable`, and so never holds a peer.
#[derive(Debug, Default)]
pub struct BrowserPeer {
    peers: PeerTable<OpenPeer>,
    events: PeerEventSink,
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

    /// Everything the browser has reported since the last drain, in arrival
    /// order. The queue is empty afterwards, so a second drain settles nothing
    /// twice.
    pub fn drain_events(&self) -> Vec<PeerEvent> {
        self.events.drain()
    }

    /// The attempts whose events were dropped for want of queue room, and the
    /// fact that they were.
    pub fn take_overflowed(&self) -> Overflowed {
        self.events.take_overflowed()
    }

    /// The attempt one open peer was opened for.
    pub fn attempt_of(&self, attempt_id: u64) -> Option<PeerAttempt> {
        self.peers.get(attempt_id).map(|peer| peer.attempt.clone())
    }

    /// Whether this attempt is still held, which is what a browser callback's
    /// event asks before it is reported to the core.
    pub fn holds(&self, attempt_id: u64) -> bool {
        self.peers.get(attempt_id).is_some()
    }

    /// Whether the browser has reported this lane open, which is the only
    /// question a write may ask before it writes.
    pub fn lane_is_open(&self, attempt_id: u64, lane: PeerLane) -> bool {
        self.peers
            .get(attempt_id)
            .is_some_and(|peer| lane_slot(peer.open_lanes, lane))
    }

    /// Record that the browser reported one lane open.
    ///
    /// Taken from the event rather than read off the channel: a channel's
    /// `readyState` is the browser's own answer to "will you take bytes now",
    /// and a host that asked it on every write would be asking once per
    /// keystroke to learn the same fact.
    pub fn mark_lane_open(&mut self, attempt_id: u64, lane: PeerLane) {
        if let Some(peer) = self.peers.get_mut(attempt_id) {
            peer.open_lanes[lane.stream_id() as usize] = true;
        }
    }

    /// The attempts this adapter holds, for the tick that drives them.
    pub fn attempt_ids(&self) -> Vec<u64> {
        self.peers.attempt_ids()
    }

    /// Start one stats read for one peer, and report whether there was one to
    /// start.
    ///
    /// Called from the pump's tick rather than from a browser callback, because
    /// `getStats` is a promise and a callback that started one would have to hold
    /// the pump's borrow until it settled. The caller owns the RATE: this answers
    /// "may I look now", never "how often should I".
    #[cfg(target_arch = "wasm32")]
    pub fn measure_attempt(&self, attempt_id: u64) -> bool {
        let Some(peer) = self.peers.get(attempt_id) else {
            return false;
        };
        stats::measure(&peer.browser.connection, attempt_id, &self.events);
        true
    }

    /// What the browser is holding for one peer's channels, which the send path
    /// is bounded by and the route diagnostic publishes.
    #[cfg(target_arch = "wasm32")]
    pub fn buffered_bytes(&self, attempt_id: u64) -> u64 {
        self.peers
            .get(attempt_id)
            .map_or(0, |peer| stats::buffered_bytes(&peer.browser.channels))
    }
}

/// The slot a lane occupies in an open peer's three-lane record.
#[must_use]
fn lane_slot(slots: [bool; 3], lane: PeerLane) -> bool {
    slots[lane.stream_id() as usize]
}

#[cfg(target_arch = "wasm32")]
impl PeerTransport for BrowserPeer {
    /// Open the three ordered channels, start ICE gathering, and install the
    /// handlers that report what the browser then does.
    ///
    /// This returns nothing to send, because there is nothing to send YET: a
    /// browser fills the local description in as candidates gather, and the
    /// gathering deadline that decides when to read it belongs to the host. The
    /// host calls `local_offer` once that deadline has passed.
    fn open(&mut self, attempt: &PeerAttempt) -> Result<(), TransportError> {
        let connection = dom::construct(&attempt.stun_urls)?;
        let channels = dom::open_lanes(&connection, &LANES)?;

        // Installed BEFORE the offer is created: a host-only gatherer can finish
        // inside the offer's own task, and a transition with no handler yet is
        // an offer nobody reads until the deadline.
        let mut handlers =
            dom::install_connection_handlers(&connection, attempt.attempt_id, &self.events);
        for (lane, _label) in LANES {
            let channel = channels
                .get(&lane.stream_id())
                .ok_or_else(interop::closed)?;
            handlers.extend(dom::install_channel_handlers(
                channel,
                lane,
                attempt.attempt_id,
                &self.events,
            ));
            handlers.extend(dom::install_message_handler(
                channel,
                lane,
                attempt.attempt_id,
                &self.events,
            ));
        }
        dom::begin_gathering(&connection, attempt.attempt_id, &self.events)?;

        let opened = OpenPeer {
            browser: dom::BrowserPeer {
                connection,
                channels,
            },
            _handlers: handlers,
            attempt: attempt.clone(),
            open_lanes: [false; 3],
        };
        // An attempt id is never reused by the core; if one were, the peer it
        // displaced is closed rather than left open behind the new one.
        if let Some(displaced) = self.peers.open(attempt.attempt_id, opened) {
            dom::close(&displaced.browser);
        }
        tracing::info!(
            target: "carriers",
            attempt_id = attempt.attempt_id,
            worker_fp = %attempt.worker_fp,
            peer_id = %attempt.peer_id,
            stun_servers = attempt.stun_urls.len(),
            open = self.peers.open_count(),
            "peer transport opened; ICE is gathering"
        );
        Ok(())
    }

    /// Read the local description, with the browser's ICE-TCP candidates
    /// removed: the offer is UDP-only before it crosses to the coordinator,
    /// exactly as v2's `filterBrowserTerminalPeerUdpCandidates` leaves it.
    fn local_offer(&self, attempt_id: u64) -> Result<String, TransportError> {
        let peer = self.peers.get(attempt_id).ok_or_else(interop::closed)?;
        let sdp = dom::gathered_sdp(&peer.browser.connection)?;
        Ok(roost_protocol::terminal_peer::sdp::filter_browser_terminal_peer_udp_candidates(&sdp))
    }

    /// Hand the coordinator's answer to the open transport. The browser settles
    /// it on a task; a rejection arrives through the sink as the attempt's end.
    fn accept_answer(&mut self, attempt_id: u64, answer_sdp: &str) -> Result<(), TransportError> {
        let peer = self.peers.get(attempt_id).ok_or_else(interop::closed)?;
        dom::apply_remote_answer(
            &peer.browser.connection,
            answer_sdp,
            attempt_id,
            &self.events,
        )
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
        let peer = self.peers.get(attempt_id).ok_or_else(interop::closed)?;
        let channel = peer
            .browser
            .channels
            .get(&lane.stream_id())
            .ok_or_else(|| interop::refused("this peer has no such lane"))?;
        dom::write(channel, bytes)
    }

    /// Write one already-framed content-free probe. Two unanswered probes retire
    /// the peer, and the count is the caller's because the reply it correlates
    /// is the caller's too.
    fn probe(&mut self, attempt_id: u64, frame: &[u8]) -> Result<(), TransportError> {
        self.send(attempt_id, PeerLane::Control, frame)
    }

    /// Close the peer and every channel on it. Closing one that is already gone
    /// is not an error: a fault path and a page teardown both reach here.
    fn close(&mut self, attempt_id: u64, _reason: &str) {
        if let Some(peer) = self.peers.close(attempt_id) {
            dom::close(&peer.browser);
            tracing::info!(
                target: "carriers",
                attempt_id,
                worker_fp = %peer.attempt.worker_fp,
                open = self.peers.open_count(),
                "peer transport closed"
            );
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

#[cfg(test)]
mod tests;
