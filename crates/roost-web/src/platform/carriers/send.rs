//! What one `SendDirect` becomes, given what this document is holding.
//!
//! Owned by `platform::carriers`, called by `pump::carriers` for every
//! `Effect::SendDirect`. It is the three steps of the send path with the last
//! one left to the caller: a LOOKUP (which connection presents this exact
//! generation — `route::CarrierTable`), a DECISION (whether the command may go
//! out — `client::carriers::deliver_direct_command`, in the core), and a WRITE.
//!
//! The decision stays in the core because the same question is asked on a
//! loopback socket and on a WebRTC lane, and two answers from two places is how a
//! command reaches a carrier whose grant does not cover it. What this function
//! adds is the one fact only the host has: the granted scope of the connection
//! that is presenting the token RIGHT NOW, which is what makes
//! `CarrierPresence::live(admits_session)` true rather than assumed.
//!
//! A connection that is present but does not admit the session is
//! `SessionNotAdmitted`, not `NoLiveCarrier`, and a host that passed `live(true)`
//! because a socket existed would turn that into bytes on the wire and a refusal
//! from the worker after a round trip spent to learn so.

use std::collections::BTreeSet;

use roost_client_core::TerminalToken;
use roost_client_core::client::carriers::{
    CarrierPresence, Delivery, deliver_direct_command, session_of,
};
use roost_client_core::effect::DirectCommand;

/// Decide what one command becomes on the connection presenting `token`.
///
/// `presenting` is that connection's granted scope, and `None` means nothing is
/// presenting the generation. It is one value rather than two booleans so a
/// caller cannot report a connection as both live and absent, which would make
/// the decision depend on argument order.
pub fn delivery(
    token: &TerminalToken,
    command: &DirectCommand,
    presenting: Option<&BTreeSet<String>>,
) -> Delivery {
    match presenting {
        None => deliver_direct_command(token, CarrierPresence::Absent, command),
        Some(granted) => deliver_direct_command(
            token,
            CarrierPresence::Live {
                admits_session: granted.contains(session_of(command)),
            },
            command,
        ),
    }
}
