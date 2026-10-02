//! One loopback carrier: a socket, the grant it spends, and the connection the
//! client core elects it as.
//!
//! Owned by `platform::carriers`, driven by `super::host`. It performs the two
//! halves of a loopback carrier's life and nothing else — the HANDSHAKE
//! (`open`, then `admit` on the worker's `Ready`) and the TRAFFIC (`send`, and
//! the `DirectInbound` each server frame becomes). Which sessions it may carry,
//! when it may open, and what happens when it dies are `client::carriers` and
//! `terminal::routes`, and this file asks neither.
//!
//! The admission is deliberately the CORE's rule and not a local one:
//! `client::local::door::admit_ready` is what refuses a `Ready` naming another
//! worker, another grant scope, or a peer id on a loopback socket, and a second
//! copy of it here would be a second answer to "may this carrier carry these
//! sessions" from a different copy of the tuple.
//!
//! Ported from `apps/web/src/store/transport/local-terminal.ts:71-270`
//! (`LoopbackTerminalConnection`).

use std::collections::BTreeSet;

use roost_client_core::DirectCarrier;
use roost_client_core::client::carriers::session_of;
use roost_client_core::client::carriers::wire::{
    DirectInbound, decode_server_frame, encode_direct_command, encode_hello,
};
use roost_client_core::client::local::door::{LoopbackAdmission, LoopbackReady, admit_ready};
use roost_client_core::client::local::{GrantSecret, LocalTerminalGrant};
use roost_client_core::effect::DirectCommand;

/// Why a loopback carrier could not be admitted, as the HOST sees it.
///
/// Distinct from `client::local::door::ReadyRefusal`, which is the rule's own
/// vocabulary: this adds the two things only the host knows — that a socket
/// failed to open at all, and that a frame could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarrierFault {
    /// The socket could not be opened.
    Dial {
        /// What the browser reported.
        detail: String,
    },
    /// The worker's `Ready` broke one of the admission rules.
    Refused {
        /// The rule it broke, spelled by the core.
        detail: String,
    },
    /// A frame arrived that no `LocalTerminalServerFrame` describes.
    Undecodable {
        /// The decoder's own reason.
        detail: String,
    },
    /// A frame arrived before the carrier authenticated.
    OutOfOrder {
        /// What arrived instead of a `Ready`.
        detail: String,
    },
    /// The worker closed the carrier.
    Closed {
        /// Its reason, or a host-supplied one when it sent none.
        detail: String,
    },
}

impl std::fmt::Display for CarrierFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dial { detail } => write!(formatter, "the loopback dial failed: {detail}"),
            Self::Refused { detail } => {
                write!(formatter, "the loopback Ready was refused: {detail}")
            }
            Self::Undecodable { detail } => {
                write!(formatter, "a loopback frame was undecodable: {detail}")
            }
            Self::OutOfOrder { detail } => {
                write!(formatter, "a loopback frame arrived out of order: {detail}")
            }
            Self::Closed { detail } => write!(formatter, "the loopback carrier closed: {detail}"),
        }
    }
}

/// A loopback carrier that has opened its socket and not yet been admitted.
///
/// The type is the state: a `LoopbackConnection` exists only once the worker has
/// proved its tuple, so nothing downstream can hold one and treat it as a
/// carrier. That is the pre-hello rule made into the type system rather than
/// into a boolean somebody has to remember to check.
#[derive(Debug)]
pub struct LoopbackConnection {
    carrier: DirectCarrier,
    admission: LoopbackAdmission,
}

impl LoopbackConnection {
    /// The carrier to register with the client core.
    pub fn carrier(&self) -> &DirectCarrier {
        &self.carrier
    }

    /// The admission the `Ready` earned, and the sessions it may serve.
    pub fn admission(&self) -> &LoopbackAdmission {
        &self.admission
    }

    /// Whether this carrier may carry a session right now.
    pub fn allows_session(&self, session_id: &str) -> bool {
        self.admission.allows_session(session_id)
    }

    /// Take the carrier out, for the core's registry.
    pub fn into_carrier(self) -> DirectCarrier {
        self.carrier
    }

    /// The bytes for one `DirectCommand`, or `None` when this carrier is not
    /// allowed to carry the session it names.
    ///
    /// A refusal is `None` rather than a silently-sent frame: a command on a
    /// session this grant does not name is a request the worker would refuse
    /// after it had already cost a round trip.
    pub fn encode(&self, command: &DirectCommand) -> Option<Vec<u8>> {
        self.allows_session(session_of(command))
            .then(|| encode_direct_command(command))
    }

    /// The `Hello` bytes this carrier spends, built from the grant it holds.
    ///
    /// `peer_id` and `worker_epoch` are EMPTY, and that is the loopback
    /// contract rather than a default: `admit_ready` reads their absence as the
    /// ROLLING-worker compatibility case and refuses a loopback socket that
    /// claims a peer.
    pub fn hello(grant: &LocalTerminalGrant) -> Vec<u8> {
        encode_hello(
            &grant.grant_id,
            &secret_text(&grant.secret),
            &grant.tab_id,
            &grant.device_fingerprint,
            "",
            "",
        )
    }

    /// Judge the worker's `Ready` against the grant that asked for it.
    ///
    /// The whole admission is this one call: the worker fingerprint, the peer
    /// id, the granted scope and the worker epoch are all checked by
    /// `client::local::door::admit_ready`, and a rolling worker that answers
    /// without an epoch or socket id gets the per-connection namespace that is
    /// the only fence it can have.
    pub fn admit(
        grant: &LocalTerminalGrant,
        door_worker_fp: &str,
        connection_id: &str,
        ready: &LoopbackReady,
    ) -> Result<Self, CarrierFault> {
        let admission =
            admit_ready(grant, door_worker_fp, connection_id, ready).map_err(|error| {
                CarrierFault::Refused {
                    detail: error.reason().to_owned(),
                }
            })?;
        let carrier = DirectCarrier {
            connection_id: connection_id.to_owned(),
            worker_fp: admission.token.worker_fp.clone().unwrap_or_default(),
            transport: admission.token.transport,
            token: admission.token.clone(),
            granted_sessions: admission.ready_sessions.clone(),
        };
        Ok(Self { carrier, admission })
    }

    /// What one server frame means on an ALREADY authenticated carrier.
    ///
    /// A free function, not a method, because the answer does not depend on the
    /// connection: `decode` is the core's, so the pre-hello and repeated-`Ready`
    /// rules are the same ones the peer path enforces and a carrier cannot have
    /// two of them.
    pub fn receive_from(bytes: &[u8]) -> Result<DirectInbound, CarrierFault> {
        decode_server_frame(bytes, true).map_err(|error| CarrierFault::Undecodable {
            detail: error.detail,
        })
    }

    /// What one server frame means BEFORE this carrier was admitted.
    ///
    /// A `Ready` is the only legal answer, and anything else is
    /// [`CarrierFault::OutOfOrder`] rather than a decode failure: the bytes were
    /// a real frame, they simply arrived before the handshake that authorises
    /// them.
    pub fn receive_pre_hello(bytes: &[u8]) -> Result<LoopbackReady, CarrierFault> {
        match decode_server_frame(bytes, false).map_err(|error| CarrierFault::Undecodable {
            detail: error.detail,
        })? {
            DirectInbound::Ready(ready) => Ok(ready),
            _ => Err(CarrierFault::OutOfOrder {
                detail: "a frame that is not a Ready arrived before the handshake".to_owned(),
            }),
        }
    }
}

/// The grant's bearer secret, as the wire carries it.
///
/// `GrantSecret` deliberately renders as `redacted` under `Debug` and has no
/// accessor, because a secret is logged, traced and propagated through `?` more
/// freely than any other value in a client. This is the ONE place it leaves the
/// wrapper, and it is a function rather than a method precisely so that reading
/// the secret is a thing a reader has to find rather than a thing any holder of
/// the value can do by accident.
fn secret_text(secret: &GrantSecret) -> String {
    secret.expose().to_owned()
}

/// The sessions a loopback carrier is registered for, sorted and exact.
///
/// Never "all": `protocol/spec/direct-terminal.md:23` makes the grant
/// scope-bound, and a registry entry that widened it would elect a route the
/// worker never authorised.
pub fn registered_sessions(admission: &LoopbackAdmission) -> BTreeSet<String> {
    admission.ready_sessions.clone()
}

/// The handshake and traffic rules, exercised. A fixture module because this
/// file is at the 400-line cap; see `loopback_carrier/tests.rs`.
#[cfg(test)]
mod tests;
