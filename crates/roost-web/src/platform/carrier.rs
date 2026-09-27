//! A direct carrier's identity: the connection id, and the `DirectCarrier` a
//! grant is assembled into.
//!
//! Owned by `platform`, called by the loopback and WebRTC carrier slices, and
//! depends on nothing but the client core's carrier types. Neither transport is
//! here — opening a loopback socket and negotiating a WebRTC session are two
//! different problems — but the two things both of them must get identically
//! right are: a connection id that names ONE connection rather than every
//! connection that ever existed, and a grant that admits exactly the sessions it
//! names.
//!
//! Ported from `apps/web/src/store/terminal-stream-transport.ts` and
//! `apps/web/src/store/transport/local-terminal.ts`; the grant scope rule is
//! `protocol/spec/direct-terminal.md:23`.

use std::collections::BTreeSet;

use roost_client_core::{DirectCarrier, TerminalToken, TerminalTransport};

/// What a host knows about a carrier the moment its transport authenticates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarrierIdentity {
    /// The host's own id for this connection.
    pub connection_id: String,
    /// The worker it reaches.
    pub worker_fp: String,
    /// Which kind of transport it is.
    pub transport: TerminalTransport,
}

impl CarrierIdentity {
    /// An identity for one connection to `worker_fp`.
    ///
    /// The id is minted once per connection and never reused: a retirement names
    /// one connection, and an id that a reconnect re-mints would retire the new
    /// connection when the old one is reported gone.
    pub fn mint(worker_fp: impl Into<String>, transport: TerminalTransport) -> Self {
        let worker_fp = worker_fp.into();
        Self {
            connection_id: mint_connection_id(&worker_fp, transport),
            worker_fp,
            transport,
        }
    }

    /// The carrier this identity becomes once the authority has granted it a
    /// token and named the sessions the grant admits.
    ///
    /// `granted_sessions` is exact and never widened to "all": a grant is
    /// scope-bound, and a carrier that admitted everything would let a pane keep
    /// painting after its grant was narrowed.
    pub fn into_carrier(
        self,
        token: TerminalToken,
        granted_sessions: impl IntoIterator<Item = String>,
    ) -> DirectCarrier {
        DirectCarrier {
            connection_id: self.connection_id,
            worker_fp: self.worker_fp,
            transport: self.transport,
            token,
            granted_sessions: granted_sessions.into_iter().collect::<BTreeSet<String>>(),
        }
    }
}

/// A connection id for one carrier of one worker.
///
/// Readable rather than random-only, because a diagnostic that has to name a
/// connection in a log line is far more useful when the worker is legible in it.
/// The suffix is what keeps it unique: two loopback sockets to the same worker
/// are two connections, and the registry keys on the id.
pub fn mint_connection_id(worker_fp: &str, transport: TerminalTransport) -> String {
    let stamp = js_sys::Date::now() as u64;
    format!(
        "{}-{}-{}-{}",
        transport.as_str(),
        sanitise(worker_fp),
        stamp,
        unique_suffix()
    )
}

/// The fingerprint with the characters a connection id may not carry, removed.
///
/// A worker fingerprint is lowercase hex, so this normally changes nothing. It
/// is here because a connection id travels into a URL, a log line and a map key,
/// and the three do not agree on which separators are safe.
fn sanitise(worker_fp: &str) -> String {
    worker_fp
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(16)
        .collect()
}

/// A per-connection discriminator from the browser's own randomness source.
fn unique_suffix() -> String {
    let entropy = js_sys::Math::random();
    if entropy.is_finite() && entropy > 0.0 {
        return format!("{}", (entropy * 1_000_000_000.0) as u64);
    }
    // `Math.random` cannot be zero in a browser, but a stubbed or seeded
    // environment can return it, and two connections with the same id would make
    // a retirement ambiguous rather than merely late.
    format!("{:x}", js_sys::Date::now() as u64)
}
