//! Which carrier a terminal tab's cells and input actually ride, as the tab
//! header shows it. A route is shown only after its elected canonical replica
//! owns a baseline, so a candidate that merely registered never leaks into UI.
//!
//! Read by roost-web's `components::terminal::terminal_transport_indicator`, the
//! pane's `data-terminal-transport` attribute, and the connection banner. Ports
//! `apps/web/src/store/local-transport-indicator.ts`.

use crate::store::Store;
use crate::store::sync_feeds::ProbeRoute;
use crate::terminal::token::{TerminalToken, TerminalTransport};

/// The header chip's content for one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportPresentation {
    /// The confirmed carrier, or `None` while nothing is confirmed.
    pub kind: Option<TerminalTransport>,
    /// The chip's label.
    pub label: &'static str,
    /// The chip's title.
    pub description: &'static str,
}

impl TransportPresentation {
    /// The `data-terminal-transport` spelling v2 renders: `webrtc` for a peer,
    /// and `unconfirmed` while no carrier is confirmed.
    pub const fn kind_attribute(&self) -> &'static str {
        match self.kind {
            None => "unconfirmed",
            Some(kind) => transport_attribute(kind),
        }
    }
}

/// The attribute spelling of one confirmed carrier.
pub const fn transport_attribute(kind: TerminalTransport) -> &'static str {
    match kind {
        TerminalTransport::Sync => "sync",
        TerminalTransport::Loopback => "loopback",
        TerminalTransport::Peer => "webrtc",
    }
}

const LOOPBACK: TransportPresentation = TransportPresentation {
    kind: Some(TerminalTransport::Loopback),
    label: "Loopback",
    description: "Terminal cells and input use a direct connection on this device.",
};
const WEBRTC: TransportPresentation = TransportPresentation {
    kind: Some(TerminalTransport::Peer),
    label: "WebRTC",
    description: "Terminal cells and input use a direct WebRTC connection to the worker.",
};
const SYNC: TransportPresentation = TransportPresentation {
    kind: Some(TerminalTransport::Sync),
    label: "Coordinator",
    description: "Terminal cells and input go through the coordinator over Sync.",
};
const WAITING: TransportPresentation = TransportPresentation {
    kind: None,
    label: "Waiting",
    description: "No transport is confirmed for the current terminal screen.",
};

/// The rule, over the three facts it reads: the replica's baseline, the
/// generation it is fenced to, and the token of the direct route elected for
/// the session. A direct generation counts only while the elected route
/// presents exactly that token: a registered connection is not a carrier.
pub fn confirmed_transport(
    baseline_ready: bool,
    generation: Option<&TerminalToken>,
    elected_route: Option<&TerminalToken>,
) -> Option<TerminalTransport> {
    let generation = generation?;
    if !baseline_ready {
        return None;
    }
    if generation.transport == TerminalTransport::Sync {
        return Some(TerminalTransport::Sync);
    }
    (elected_route == Some(generation)).then_some(generation.transport)
}

/// The confirmed carrier of one session's replica, or `None`.
pub fn session_terminal_transport_kind(
    store: &Store,
    session_id: &str,
) -> Option<TerminalTransport> {
    let replica = store.terminal.get(session_id)?;
    confirmed_transport(
        replica.baseline_ready(),
        replica.generation(),
        store.routes.route(session_id).map(|route| &route.token),
    )
}

/// The chip content for one session.
pub fn session_terminal_transport_presentation(
    store: &Store,
    session_id: &str,
) -> TransportPresentation {
    presentation_for(session_terminal_transport_kind(store, session_id))
}

/// The chip content for a confirmed carrier, or the waiting chip.
pub const fn presentation_for(kind: Option<TerminalTransport>) -> TransportPresentation {
    match kind {
        None => WAITING,
        Some(TerminalTransport::Sync) => SYNC,
        Some(TerminalTransport::Loopback) => LOOPBACK,
        Some(TerminalTransport::Peer) => WEBRTC,
    }
}

/// Whether any elected direct route still holds a current terminal proof: the
/// replica's own generation is direct, owns a baseline, has accepted a frame on
/// that generation, and is exactly the route elected for the session. A peer
/// counts only while its last answered probe is inside the qualification window
/// — a peer that stopped answering is not a terminal the reader still has.
///
/// The coordinator-outage banner reads this: a live direct terminal makes an
/// outage a partial degradation rather than "sessions paused". Ports v2
/// `hasLivenessQualifiedDirectTerminal`.
pub fn has_liveness_qualified_direct_terminal(store: &Store) -> bool {
    store.terminal.iter().any(|(session_id, replica)| {
        let Some(token) = replica.generation() else {
            return false;
        };
        token.transport != TerminalTransport::Sync
            && replica.baseline_ready()
            && replica.liveness().last_accepted_at_ms().is_some()
            && store.routes.route(session_id).map(|route| &route.token) == Some(token)
            && (token.transport != TerminalTransport::Peer || peer_is_qualified(store, token))
    })
}

/// A peer answered its newest probe inside the qualification window.
fn peer_is_qualified(store: &Store, token: &TerminalToken) -> bool {
    token.worker_fp.as_deref().is_some_and(|worker_fp| {
        store
            .direct
            .snapshot(worker_fp)
            .telemetry
            .liveness_qualified
    })
}

/// The round trip already measured on the carrier a session's input rides,
/// which a pane's predictive echo may adopt before its first echo is timed: a
/// peer's own probe on a direct route, the worker's control probe over Sync.
/// A loopback route is never slow enough to show a guess, so it seeds nothing.
pub fn session_route_rtt_ms(store: &Store, session_id: &str, worker_fp: &str) -> Option<u64> {
    match session_terminal_transport_kind(store, session_id)? {
        TerminalTransport::Loopback => None,
        TerminalTransport::Peer => store.direct.snapshot(worker_fp).telemetry.rtt_ms,
        TerminalTransport::Sync => store
            .transport_probes
            .get(worker_fp)
            .filter(|probe| matches!(probe.route, ProbeRoute::Sync { .. }))
            .map(|probe| probe.control_rtt_ms),
    }
}
