//! Which live connection presents a given carrier generation.
//!
//! Owned by `platform::carriers`, called by `pump::effects` for every
//! `Effect::SendDirect`. It is a LOOKUP and deliberately not a decision: the
//! question "may this carrier carry this session" belongs to
//! `terminal::routes` and `client::local::door::admit_ready`, and a table that
//! answered it would be a second answer arriving from a different copy of the
//! tuple — the same drift the token's own fields exist to prevent.
//!
//! The key is the token's IDENTITY, not the connection id, because the id names
//! one socket and the identity names one generation. Two carriers for the same
//! worker on the same transport are not a thing `protocol/spec/direct-terminal.md`
//! admits, so a second registration REPLACES the first and the displaced
//! connection id is handed back — which is what lets the caller retire exactly
//! the one the registry was serving.
//!
//! Ported from the `TerminalDirectRegistry` slot bookkeeping in
//! `apps/web/src/store/terminal-stream-transport.ts:119-180`.

use std::collections::BTreeMap;

use roost_client_core::TerminalToken;
use roost_client_core::TerminalTransport;

/// The identity one carrier generation is reached by.
///
/// `process_epoch` is in the key and not derived: a worker restart changes it,
/// which is exactly what must stop a frame or a keystroke meant for the old
/// process from reaching whatever took its place.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConnectionKey {
    /// The worker.
    pub worker_fp: String,
    /// Which kind of carrier.
    pub transport: TerminalTransport,
    /// The worker process epoch.
    pub process_epoch: String,
}

impl ConnectionKey {
    /// The key a token names, or `None` for a token that names no worker — a
    /// Sync token, which no direct connection can ever present.
    pub fn of(token: &TerminalToken) -> Option<Self> {
        Some(Self {
            worker_fp: token.worker_fp.clone()?,
            transport: token.transport,
            process_epoch: token.process_epoch.clone(),
        })
    }
}

/// One document's live direct connections, by the generation each presents.
#[derive(Debug)]
pub struct CarrierTable<T> {
    connections: BTreeMap<ConnectionKey, (String, TerminalToken, T)>,
}

impl<T> Default for CarrierTable<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> CarrierTable<T> {
    /// A table with no connections.
    pub fn new() -> Self {
        Self {
            connections: BTreeMap::new(),
        }
    }

    /// Register a connection, returning the connection id it DISPLACED.
    ///
    /// A displacement is not an error and is not silent: the caller retires the
    /// returned id, so a worker that reconnects on a new epoch loses the old
    /// epoch's connection rather than leaving it registered beside the new one.
    pub fn register(
        &mut self,
        key: ConnectionKey,
        connection_id: impl Into<String>,
        token: TerminalToken,
        connection: T,
    ) -> Option<String> {
        self.connections
            .insert(key, (connection_id.into(), token, connection))
            .map(|(displaced, _, _)| displaced)
    }

    /// The connection presenting exactly this generation.
    pub fn get(&self, token: &TerminalToken) -> Option<&T> {
        Some(&self.connections.get(&ConnectionKey::of(token)?)?.2)
    }

    /// The connection presenting exactly this generation, mutably.
    pub fn get_mut(&mut self, token: &TerminalToken) -> Option<&mut T> {
        Some(&mut self.connections.get_mut(&ConnectionKey::of(token)?)?.2)
    }

    /// The host's id for the connection presenting this generation.
    pub fn connection_id(&self, token: &TerminalToken) -> Option<&str> {
        Some(&self.connections.get(&ConnectionKey::of(token)?)?.0)
    }

    /// Drop one connection by the host's id for it, and hand its connection back.
    ///
    /// Keyed on the ID and not the key on purpose: a retirement names ONE
    /// connection, and a key that has already been reused by a newer epoch must
    /// not lose the newer connection to a close belonging to the older one.
    pub fn retire(&mut self, connection_id: &str) -> Option<T> {
        let key = self
            .connections
            .iter()
            .find(|(_, (id, _, _))| id == connection_id)
            .map(|(key, _)| key.clone())?;
        self.connections
            .remove(&key)
            .map(|(_, _, connection)| connection)
    }

    /// Drop every connection for a worker, for a retirement or a restart.
    pub fn retire_worker(&mut self, worker_fp: &str) -> Vec<(String, TerminalToken, T)> {
        let doomed: Vec<ConnectionKey> = self
            .connections
            .keys()
            .filter(|key| key.worker_fp == worker_fp)
            .cloned()
            .collect();
        doomed
            .into_iter()
            .filter_map(|key| self.connections.remove(&key))
            .collect()
    }

    /// The connection registered under this host id.
    pub fn get_by_connection(&self, connection_id: &str) -> Option<&T> {
        self.connections
            .values()
            .find(|(id, _, _)| id == connection_id)
            .map(|(_, _, connection)| connection)
    }

    /// The generation the connection registered under this host id presents.
    ///
    /// The drain is handed a connection id by a socket callback, which knows
    /// nothing about the generation; without this it would have to guess which
    /// token a frame belongs to, and a guessed token is a frame folded against
    /// the wrong route.
    pub fn token_of(&self, connection_id: &str) -> Option<&TerminalToken> {
        self.connections
            .iter()
            .find(|(_, (id, _, _))| id == connection_id)
            .map(|(_, (_, token, _))| token)
    }

    /// Whether a connection with this host id is registered.
    ///
    /// The drain needs this and not a key: it is handed a connection id by a
    /// socket callback, which knows nothing about the generation that
    /// generation is keyed by.
    pub fn contains(&self, connection_id: &str) -> bool {
        self.connections
            .values()
            .any(|(id, _, _)| id == connection_id)
    }

    /// How many connections this document holds, for the document-wide peer cap
    /// that `client::carriers::Signalling` enforces.
    pub fn len(&self) -> usize {
        self.connections.len()
    }

    /// Whether this document holds no direct connection at all.
    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn direct(worker: &str, epoch: &str, transport: TerminalTransport) -> TerminalToken {
        TerminalToken::direct(7, transport, worker, epoch, 7)
    }

    fn key(worker: &str, epoch: &str, transport: TerminalTransport) -> ConnectionKey {
        ConnectionKey {
            worker_fp: worker.to_owned(),
            transport,
            process_epoch: epoch.to_owned(),
        }
    }

    #[test]
    fn a_command_reaches_the_connection_whose_generation_it_names() {
        let mut table = CarrierTable::new();
        table.register(
            key("worker-a", "epoch-a", TerminalTransport::Loopback),
            "conn-a",
            direct("worker-a", "epoch-a", TerminalTransport::Loopback),
            1,
        );
        assert_eq!(
            table.get(&direct("worker-a", "epoch-a", TerminalTransport::Loopback)),
            Some(&1)
        );
    }

    #[test]
    fn a_command_for_a_different_epoch_never_reaches_the_old_connection() {
        // The failure this table exists to prevent: a worker restarts, a new
        // epoch is registered, and a keystroke admitted against the OLD one is
        // written into a PTY the new process is already serving.
        let mut table = CarrierTable::new();
        table.register(
            key("worker-a", "epoch-a", TerminalTransport::Loopback),
            "conn-a",
            direct("worker-a", "epoch-a", TerminalTransport::Loopback),
            1,
        );
        table.register(
            key("worker-a", "epoch-b", TerminalTransport::Loopback),
            "conn-b",
            direct("worker-a", "epoch-b", TerminalTransport::Loopback),
            2,
        );
        assert_eq!(
            table.get(&direct("worker-a", "epoch-a", TerminalTransport::Loopback)),
            Some(&1)
        );
        assert_eq!(
            table.get(&direct("worker-a", "epoch-b", TerminalTransport::Loopback)),
            Some(&2)
        );
    }

    #[test]
    fn a_reconnect_on_the_same_epoch_names_the_connection_it_displaced() {
        let mut table = CarrierTable::new();
        assert_eq!(
            table.register(
                key("worker-a", "epoch-a", TerminalTransport::Loopback),
                "conn-a",
                direct("worker-a", "epoch-a", TerminalTransport::Loopback),
                1
            ),
            None
        );
        assert_eq!(
            table.register(
                key("worker-a", "epoch-a", TerminalTransport::Loopback),
                "conn-b",
                direct("worker-a", "epoch-a", TerminalTransport::Loopback),
                2
            ),
            Some("conn-a".to_owned()),
            "a replacement must be told which connection it displaced, or the \
             old one stays registered beside it"
        );
        assert_eq!(
            table.connection_id(&direct("worker-a", "epoch-a", TerminalTransport::Loopback)),
            Some("conn-b")
        );
    }

    #[test]
    fn a_close_from_a_displaced_connection_does_not_retire_its_replacement() {
        let mut table = CarrierTable::new();
        table.register(
            key("worker-a", "epoch-a", TerminalTransport::Loopback),
            "conn-a",
            direct("worker-a", "epoch-a", TerminalTransport::Loopback),
            1,
        );
        let displaced = table.register(
            key("worker-a", "epoch-a", TerminalTransport::Loopback),
            "conn-b",
            direct("worker-a", "epoch-a", TerminalTransport::Loopback),
            2,
        );
        assert_eq!(displaced, Some("conn-a".to_owned()));
        // The late close belongs to the socket that is already gone.
        assert_eq!(table.retire("conn-a"), None);
        assert_eq!(
            table.get(&direct("worker-a", "epoch-a", TerminalTransport::Loopback)),
            Some(&2)
        );
    }

    #[test]
    fn a_loopback_and_a_peer_for_one_worker_are_two_generations() {
        let mut table = CarrierTable::new();
        table.register(
            key("worker-a", "epoch-a", TerminalTransport::Loopback),
            "conn-loop",
            direct("worker-a", "epoch-a", TerminalTransport::Loopback),
            1,
        );
        table.register(
            key("worker-a", "epoch-a", TerminalTransport::Peer),
            "conn-peer",
            direct("worker-a", "epoch-a", TerminalTransport::Peer),
            2,
        );
        assert_eq!(table.len(), 2);
        assert_eq!(
            table.get(&direct("worker-a", "epoch-a", TerminalTransport::Peer)),
            Some(&2)
        );
    }

    #[test]
    fn a_sync_token_names_no_connection_at_all() {
        // Sync is the fallback, never a direct carrier: a `SendDirect` naming a
        // Sync token has nothing to route to and must find nothing rather than
        // matching a carrier by accident.
        let table: CarrierTable<u8> = CarrierTable::new();
        let sync = TerminalToken::sync(1, "socket-a", "epoch-a", 1);
        assert!(ConnectionKey::of(&sync).is_none());
        assert_eq!(table.get(&sync), None);
    }

    #[test]
    fn a_worker_retirement_takes_every_connection_it_held() {
        let mut table = CarrierTable::new();
        table.register(
            key("worker-a", "epoch-a", TerminalTransport::Loopback),
            "conn-a",
            direct("worker-a", "epoch-a", TerminalTransport::Loopback),
            1,
        );
        table.register(
            key("worker-a", "epoch-a", TerminalTransport::Peer),
            "conn-peer",
            direct("worker-a", "epoch-a", TerminalTransport::Peer),
            2,
        );
        table.register(
            key("worker-b", "epoch-c", TerminalTransport::Loopback),
            "conn-c",
            direct("worker-b", "epoch-c", TerminalTransport::Loopback),
            3,
        );
        let retired = table.retire_worker("worker-a");
        assert_eq!(retired.len(), 2);
        assert_eq!(table.len(), 1);
        assert!(
            table
                .get(&direct("worker-b", "epoch-c", TerminalTransport::Loopback))
                .is_some()
        );
    }
}
