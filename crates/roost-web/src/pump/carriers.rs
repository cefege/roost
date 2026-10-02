//! The direct carriers this document holds, and what happens to one command
//! addressed to them.
//!
//! Owned by `pump`, driven by `pump::effects` for the two effects that name a
//! carrier and by `pump::carrier_dial` for the mint→socket→admission pipeline.
//! It is the host half of the seam `roost_client_core::client::carriers` draws:
//! the core decides WHEN a carrier may open, what it may carry and when it is
//! elected; this holds the live connections, writes to them, and reports what it
//! saw back as `ClientEvent`s.
//!
//! It holds no election policy. `platform::carriers::route::CarrierTable` is a
//! lookup, and whether a command may go out is
//! `client::carriers::deliver_direct_command` — in the core, because the same
//! question is asked on a loopback socket and on a WebRTC lane, and two answers
//! from two places is how a command reaches a carrier whose grant does not cover
//! it.
//!
//! THE ONE RULE THIS FILE EXISTS TO KEEP: a command that cannot be delivered is
//! REPORTED, never dropped. A drop is indistinguishable from a carrier that
//! carried the bytes and had them lost, so the client cannot tell a working path
//! from a dead one and every symptom after it is a rendering bug that looks like
//! nothing at all. Every refusal below is a `tracing` event and, where the core
//! has an event for it, a `ClientEvent`.
//!
//! MINTING the credential a carrier spends is `carriers::mint`: this file holds
//! the connections and writes to them, and the mint is the one thing here that
//! opens nothing.
//!
//! Ported from the two arms of `apps/web/src/store/transport/local-terminal.ts`
//! and the send path of `terminal-peer-connection.ts`.

mod mint;

#[cfg(target_arch = "wasm32")]
pub(super) use mint::report_mint_refusal;
pub(super) use mint::request_grant;

use roost_client_core::TerminalToken;
#[cfg(target_arch = "wasm32")]
use roost_client_core::TerminalTransport;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::carriers::Delivery;
use roost_client_core::client::carriers::SendFault;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::local::LocalTerminalGrant;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::local::discovery::DoorDiscovery;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::local::discovery::LocalWorkerDoor;
use roost_client_core::effect::DirectCommand;

use super::Pump;
#[cfg(target_arch = "wasm32")]
use crate::platform::carriers::LoopbackConnection;
#[cfg(target_arch = "wasm32")]
use crate::platform::carriers::delivery;
#[cfg(target_arch = "wasm32")]
use crate::platform::carriers::route::{CarrierTable, ConnectionKey};
#[cfg(target_arch = "wasm32")]
use crate::platform::loopback::{LoopbackHandle, LoopbackMessage};

/// Every live direct connection this document holds.
///
/// Empty in a build with no browser, which is not a degraded mode: there is no
/// carrier to hold, and every command addressed to one is refused by name.
#[derive(Debug, Default)]
pub(super) struct Carriers {
    #[cfg(target_arch = "wasm32")]
    table: CarrierTable<LiveCarrier>,
    /// Sockets that have spent a credential but have not been admitted.
    ///
    /// They live here rather than in `table` because a table entry is a
    /// CARRIER: `RouteRegistry` would elect a route for a socket that has not
    /// proved its tuple, which is the one thing the handshake exists to prevent.
    /// The connection id is the caller's so the drain can find its own socket.
    #[cfg(target_arch = "wasm32")]
    pending: std::collections::BTreeMap<String, Pending>,
    /// Which worker door, if any, this page has adopted. Memoized in the core's
    /// own object rather than in a second one here.
    #[cfg(target_arch = "wasm32")]
    doors: DoorDiscovery,
}

/// An admitted carrier: the socket.
///
/// Its granted scope is NOT copied here. The send path asks the core's
/// `RouteRegistry::granted_sessions_for`, the one copy a refreshed grant widens;
/// a second copy here would keep refusing the sessions the widened grant added.
#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub(super) struct LiveCarrier {
    handle: LoopbackHandle,
}

/// A socket awaiting its `Ready`, and the grant it is spending.
#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub(super) struct Pending {
    /// The open socket, moved into the table when the handshake completes.
    pub(super) handle: LoopbackHandle,
    /// The credential this socket is spending.
    pub(super) grant: LocalTerminalGrant,
    /// The worker that answered at the door, which is the tuple `admit_ready`
    /// judges the `Ready` against.
    pub(super) door_worker_fp: String,
}

impl Carriers {
    /// Adopt this page's worker door, or learn that there is none.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn doors(&mut self) -> &mut DoorDiscovery {
        &mut self.doors
    }

    /// The socket presenting this generation.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn handle_for(&self, token: &TerminalToken) -> Option<&LoopbackHandle> {
        self.table.get(token).map(|live| &live.handle)
    }

    /// The generation the connection registered under this host id presents.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn token_for(&self, connection_id: &str) -> Option<&TerminalToken> {
        self.table.token_of(connection_id)
    }

    /// The door this page has adopted, once discovery has answered.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn door(&self) -> Option<&LocalWorkerDoor> {
        self.doors.door()
    }

    /// Register a freshly admitted carrier, retiring whatever it displaced.
    ///
    /// The displaced connection is closed BEFORE the new one is announced,
    /// because a worker has one authority: leaving the old socket registered
    /// beside the new one is how two connections end up writing to one PTY.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn admit(
        &mut self,
        connection: &LoopbackConnection,
        handle: LoopbackHandle,
    ) -> Option<LoopbackHandle> {
        let key = ConnectionKey::of(&connection.carrier().token)?;
        let displaced = self.table.register(
            key,
            handle.connection_id().to_owned(),
            connection.carrier().token.clone(),
            LiveCarrier { handle },
        );
        self.close_displaced(displaced)
    }

    /// Record a socket that has opened but not yet been admitted.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn expect_ready(
        &mut self,
        connection_id: String,
        handle: LoopbackHandle,
        grant: LocalTerminalGrant,
        door_worker_fp: String,
    ) {
        self.pending.insert(
            connection_id,
            Pending {
                handle,
                grant,
                door_worker_fp,
            },
        );
    }

    /// Everything one socket observed, and whether that socket is a carrier yet.
    ///
    /// One call for both halves because the drain asks the same question of the
    /// same id: a `Ready` is the last thing a pending socket produces and the
    /// first thing an admitted one must never see again.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn observe(&self, connection_id: &str) -> Option<(Vec<LoopbackMessage>, bool)> {
        if let Some(pending) = self.pending.get(connection_id) {
            return Some((pending.handle.drain(), false));
        }
        self.table
            .get_by_connection(connection_id)
            .map(|live| (live.handle.drain(), true))
    }

    /// Take the pre-handshake record for a socket, so it can be admitted.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn take_pending(&mut self, connection_id: &str) -> Option<Pending> {
        self.pending.remove(connection_id)
    }

    /// Drop a socket that never authenticated, and hand it back to be closed.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn drop_pending(&mut self, connection_id: &str) -> Option<LoopbackHandle> {
        self.pending
            .remove(connection_id)
            .map(|pending| pending.handle)
    }

    /// The sockets dialled for `worker_fp` that have not authenticated yet.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn pending_for(&self, worker_fp: &str) -> Vec<String> {
        self.pending
            .iter()
            .filter(|(_, pending)| pending.grant.worker_fp == worker_fp)
            .map(|(connection_id, _)| connection_id.clone())
            .collect()
    }

    /// Drop an admitted carrier, and hand back its socket and the token it was
    /// presenting.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn retire(
        &mut self,
        connection_id: &str,
    ) -> Option<(LoopbackHandle, TerminalToken)> {
        let token = self.table.token_of(connection_id)?.clone();
        let live = self.table.retire(connection_id)?;
        Some((live.handle, token))
    }

    /// Drop every connection a worker held, for a retirement or a restart.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn retire_worker(&mut self, worker_fp: &str) -> Vec<LoopbackHandle> {
        self.table
            .retire_worker(worker_fp)
            .into_iter()
            .map(|(_connection_id, _token, live)| live.handle)
            .collect()
    }

    /// Close the connection a new carrier displaced, if there was one.
    #[cfg(target_arch = "wasm32")]
    fn close_displaced(&mut self, displaced: Option<String>) -> Option<LoopbackHandle> {
        let displaced = displaced?;
        let old = self.table.retire(&displaced)?;
        old.handle
            .close(1000, "carrier displaced by a newer connection");
        Some(old.handle)
    }
}

/// Perform one `Effect::SendDirect`.
///
/// The three steps are a lookup, a decision, and a write, and the decision is
/// the core's so that it cannot differ between the two transports. A refusal at
/// any of the three is reported with the generation it happened on, because a
/// fault with no generation in it cannot be traced to a route.
pub(super) fn send(pump: &Pump, token: &TerminalToken, command: &DirectCommand) {
    if let Err(fault) = try_send(pump, token, command) {
        report_fault(pump, token, &fault);
    }
}

/// Write one command on the carrier presenting `token`, or say why it did not
/// go out. For a caller whose own request waits on the answer: a refusal is
/// that caller's to settle, not only a line in the log.
pub(super) fn try_send(
    pump: &Pump,
    token: &TerminalToken,
    command: &DirectCommand,
) -> Result<(), SendFault> {
    #[cfg(target_arch = "wasm32")]
    if token.transport == TerminalTransport::Peer {
        return send_on_peer(pump, token, command);
    }
    // The scope is read from the core and the lookup and the write happen under
    // ONE borrow of the table, released before anything is reported: a report
    // that re-entered the pump while the table was borrowed would be a second
    // borrow of one struct.
    #[cfg(target_arch = "wasm32")]
    let granted = granted_scope(pump, token);
    let carriers = pump.inner.carriers.borrow_mut();
    #[cfg(target_arch = "wasm32")]
    {
        match delivery(token, command, granted.as_ref()) {
            Delivery::Encoded(bytes) => match carriers.handle_for(token) {
                Some(handle) if handle.send(&bytes) => Ok(()),
                // A carrier that decoded the command and could not write it is
                // the same fact to the route as one that was never there, and
                // the same repair.
                _ => Err(no_live_carrier(token)),
            },
            Delivery::Refused(fault) => Err(fault),
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (command, &carriers);
        Err(no_live_carrier(token))
    }
}

/// Write one command on the peer carrier presenting `token`.
///
/// What differs from the loopback socket is the last step: a peer routes the
/// bytes through its attempt's lane framing and the browser's channels.
#[cfg(target_arch = "wasm32")]
fn send_on_peer(
    pump: &Pump,
    token: &TerminalToken,
    command: &DirectCommand,
) -> Result<(), SendFault> {
    let granted = granted_scope(pump, token);
    match delivery(token, command, granted.as_ref()) {
        Delivery::Encoded(bytes) => super::peer_lane::write_direct(pump, token, bytes)
            // A carrier that framed the command and could not write it is the
            // same fact to the route as one that was never there.
            .map_err(|_| no_live_carrier(token)),
        Delivery::Refused(fault) => Err(fault),
    }
}

/// The exact sessions the carrier presenting `token` may carry, from the core.
#[cfg(target_arch = "wasm32")]
fn granted_scope(pump: &Pump, token: &TerminalToken) -> Option<std::collections::BTreeSet<String>> {
    pump.inner
        .core
        .borrow()
        .store()
        .routes
        .granted_sessions_for(token)
        .cloned()
}

/// The fault a generation nothing is presenting becomes.
fn no_live_carrier(token: &TerminalToken) -> SendFault {
    SendFault::NoLiveCarrier {
        worker_fp: token.worker_fp.clone().unwrap_or_default(),
        transport: token.transport,
        process_epoch: token.process_epoch.clone(),
    }
}

/// The one place a delivery failure becomes visible.
///
/// `warn`, not `error`: a command with no live carrier is the NORMAL state of a
/// session whose peer is still negotiating and whose loopback probe has not
/// answered, and logging that as an error is how a fallback path becomes noise
/// nobody reads. It is still a report, which is the property that matters.
pub(super) fn report_fault(pump: &Pump, token: &TerminalToken, fault: &SendFault) {
    tracing::warn!(
        target: "carriers",
        worker_fp = token.worker_fp.as_deref().unwrap_or(""),
        transport = token.transport.as_str(),
        process_epoch = %token.process_epoch,
        socket_generation = token.socket_generation,
        fault = %fault,
        "direct command refused; the route is not carried by a live connection"
    );
    let _ = pump;
}
