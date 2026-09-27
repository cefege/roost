//! The coordinator's bind lifecycle: turning the operator's bind string into a
//! socket address, and refusing the ones that are not one.
//!
//! Owned by the coordinator's HTTP layer. `serve` calls [`resolve_bind`] before
//! anything is bound, so a mistyped bind fails before a database is opened; the
//! listener itself never parses a bind. The admission gate's allowlist is built
//! from the port this returns, once the OS has reported which one it used.

use std::net::SocketAddr;

/// Resolve a bind string into a socket address, refusing anything unparseable.
///
/// Refusing here rather than falling back to a default is deliberate: a
/// mistyped bind that silently became `127.0.0.1:4113` would start a second
/// coordinator on the same port, and the error the operator sees would be
/// `address in use` rather than the bind they wrote.
pub fn resolve_bind(bind: &str) -> Result<SocketAddr, BindError> {
    bind.parse::<SocketAddr>().map_err(|error| BindError {
        bind: bind.to_string(),
        reason: error.to_string(),
    })
}

/// A bind that could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("ROOST_COORDINATOR_BIND {bind} is not a host:port address: {reason}")]
pub struct BindError {
    /// The bind as written.
    pub bind: String,
    /// Why it could not be resolved.
    pub reason: String,
}
