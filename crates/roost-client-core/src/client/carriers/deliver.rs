//! Whether one `DirectCommand` goes out, and — when it does not — which named
//! fault the client is told about.
//!
//! Owned by `client::carriers`. It exists because the alternative is the failure
//! this block was opened for: `Effect::SendDirect` reaching a host with no live
//! connection and being dropped, which is indistinguishable from a carrier that
//! carried the bytes and had them lost. A silent drop makes a dead path look
//! like a working one, and every symptom after it is a rendering bug that looks
//! like nothing at all.
//!
//! So every outcome is a VALUE. The three refusals are distinct because they
//! call for different repairs and the client has to be able to tell them apart:
//!
//! - [`SendFault::NoGeneration`] — the command names a generation no direct
//!   carrier can present, which for a Sync token means the caller elected the
//!   wrong route entirely;
//! - [`SendFault::NoLiveCarrier`] — the generation is a real direct one and
//!   nothing is presenting it, so the carrier is gone and the route must be
//!   retired rather than retried onto;
//! - [`SendFault::SessionNotAdmitted`] — a carrier IS presenting it and its
//!   grant does not name the session, so the bytes must not go out even though
//!   the socket is open.
//!
//! Presence is passed as one value rather than two booleans so a caller cannot
//! report a connection as both live and absent, which would make the decision
//! depend on argument order.

use crate::effect::DirectCommand;
use crate::terminal::token::{TerminalToken, TerminalTransport};

use super::wire::encode_direct_command;

/// What the host knows about the connection that should carry a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarrierPresence {
    /// A live connection presents EXACTLY this generation, and its grant admits
    /// `admits_session`.
    Live {
        /// Whether that connection's grant names the session the command is for.
        admits_session: bool,
    },
    /// Nothing presents this generation.
    Absent,
}

impl CarrierPresence {
    /// The presence of a connection the host found for this generation.
    pub const fn live(admits_session: bool) -> Self {
        Self::Live { admits_session }
    }

    /// The presence of a document holding no such connection.
    pub const ABSENT: Self = Self::Absent;
}

/// Why a command did not go out.
///
/// Never a bare `bool`: each member names a different repair, and the client
/// surfaces all three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendFault {
    /// The token names no worker, so it is a Sync generation and no direct
    /// carrier can ever present it.
    NoGeneration,
    /// The generation is a real direct one and nothing is presenting it.
    NoLiveCarrier {
        /// The worker whose carrier is gone.
        worker_fp: String,
        /// Which kind of carrier it was.
        transport: TerminalTransport,
        /// The worker process epoch it presented.
        process_epoch: String,
    },
    /// A carrier is presenting the generation and its grant does not name the
    /// session the command is for.
    SessionNotAdmitted {
        /// The session the command named.
        session_id: String,
    },
}

impl std::fmt::Display for SendFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoGeneration => write!(
                formatter,
                "a direct command named a generation no direct carrier can present"
            ),
            Self::NoLiveCarrier {
                worker_fp,
                transport,
                process_epoch,
            } => write!(
                formatter,
                "no live {} carrier presents {worker_fp}@{process_epoch}",
                transport.as_str(),
            ),
            Self::SessionNotAdmitted { session_id } => {
                write!(formatter, "the carrier's grant does not name {session_id}")
            }
        }
    }
}

impl std::error::Error for SendFault {}

/// What one `DirectCommand` did, or why it did not go out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// The frame bytes, ready to write.
    Encoded(Vec<u8>),
    /// It did not go out, and this is why. Reported, never dropped.
    Refused(SendFault),
}

impl Delivery {
    /// The bytes, or `None` when the command was refused.
    ///
    /// Present for the caller that genuinely has nothing to do with a refusal
    /// beyond stopping. The refusal itself is not discarded with the `None`.
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Encoded(bytes) => Some(bytes),
            Self::Refused(_) => None,
        }
    }

    /// The fault, or `None` when the command went out.
    pub fn fault(&self) -> Option<&SendFault> {
        match self {
            Self::Encoded(_) => None,
            Self::Refused(fault) => Some(fault),
        }
    }
}

/// The session one command is for, which is what a grant's scope is checked
/// against.
///
/// `pub` because a host holding a live connection has to answer the same
/// question to build a truthful `CarrierPresence`, and a second three-arm match
/// over `DirectCommand` in a client is how that answer starts to drift.
pub fn session_of(command: &DirectCommand) -> &str {
    match command {
        DirectCommand::View { session_id, .. }
        | DirectCommand::Resync { session_id, .. }
        | DirectCommand::Input { session_id, .. }
        | DirectCommand::RouteClaim { session_id, .. }
        | DirectCommand::Scrollback { session_id, .. } => session_id,
    }
}

/// Decide whether `command` goes out on the carrier presenting `token`.
///
/// This is the whole of the host's send path decision, and it is deliberately
/// here rather than in the host: the same question is asked on the loopback
/// socket and on a WebRTC lane, and two answers to "may this command go out"
/// from two places is how a command ends up on a carrier whose grant does not
/// cover it.
pub fn deliver_direct_command(
    token: &TerminalToken,
    presence: CarrierPresence,
    command: &DirectCommand,
) -> Delivery {
    // A Sync generation is refused before presence is even read: no direct
    // carrier can present one, so "absent" is not the interesting fact about it.
    let Some(worker_fp) = token.worker_fp.as_deref() else {
        return Delivery::Refused(SendFault::NoGeneration);
    };
    if worker_fp.is_empty() {
        return Delivery::Refused(SendFault::NoGeneration);
    }
    match presence {
        CarrierPresence::Absent => Delivery::Refused(SendFault::NoLiveCarrier {
            worker_fp: worker_fp.to_owned(),
            transport: token.transport,
            process_epoch: token.process_epoch.clone(),
        }),
        CarrierPresence::Live { admits_session } => {
            if admits_session {
                Delivery::Encoded(encode_direct_command(command))
            } else {
                Delivery::Refused(SendFault::SessionNotAdmitted {
                    session_id: session_of(command).to_owned(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use roost_proto::__buffa::oneof::local_terminal_client_frame::Frame as ClientFrame;
    use roost_proto::LocalTerminalClientFrame;
    use roost_proto::buffa::Message;

    use super::*;

    fn direct(worker: &str, epoch: &str) -> TerminalToken {
        TerminalToken::direct(7, TerminalTransport::Loopback, worker, epoch, 7)
    }

    fn resync(session: &str) -> DirectCommand {
        DirectCommand::Resync {
            session_id: session.to_owned(),
            view_id: "view-a".to_owned(),
            stream_id: String::new(),
            grid_epoch: String::new(),
            seq: 0,
        }
    }

    /// The property Main asked to be pinned as a test rather than prose: a
    /// command with nothing presenting its generation is REPORTED, and the
    /// report names the worker, the transport and the epoch that went away —
    /// everything a repair needs and nothing a silent drop could have said.
    #[test]
    fn a_command_with_no_live_carrier_is_a_named_fault_not_a_dropped_frame() {
        let delivery = deliver_direct_command(
            &direct("worker-a", "epoch-a"),
            CarrierPresence::ABSENT,
            &resync("session-a"),
        );
        assert_eq!(
            delivery.fault(),
            Some(&SendFault::NoLiveCarrier {
                worker_fp: "worker-a".to_owned(),
                transport: TerminalTransport::Loopback,
                process_epoch: "epoch-a".to_owned(),
            }),
            "an unroutable command must produce a report, not an absent result"
        );
        assert!(delivery.bytes().is_none());
        // And it reads as something an operator can act on.
        assert!(
            delivery.fault().unwrap().to_string().contains("epoch-a"),
            "the fault must name the epoch that went away, or the log cannot \
             distinguish a restart from a teardown"
        );
    }

    #[test]
    fn the_three_refusals_stay_three_distinct_faults() {
        let sync = TerminalToken::sync(1, "socket-a", "epoch-a", 1);
        assert_eq!(
            deliver_direct_command(&sync, CarrierPresence::live(true), &resync("session-a"))
                .fault(),
            Some(&SendFault::NoGeneration)
        );
        assert_eq!(
            deliver_direct_command(
                &direct("worker-a", "epoch-a"),
                CarrierPresence::ABSENT,
                &resync("session-a"),
            )
            .fault(),
            Some(&SendFault::NoLiveCarrier {
                worker_fp: "worker-a".to_owned(),
                transport: TerminalTransport::Loopback,
                process_epoch: "epoch-a".to_owned(),
            })
        );
        assert_eq!(
            deliver_direct_command(
                &direct("worker-a", "epoch-a"),
                CarrierPresence::live(false),
                &resync("session-b"),
            )
            .fault(),
            Some(&SendFault::SessionNotAdmitted {
                session_id: "session-b".to_owned(),
            })
        );
    }

    #[test]
    fn an_open_carrier_does_not_license_a_session_its_grant_does_not_name() {
        // The distinction the two "live" refusals exist to keep: the socket is
        // up, and the bytes still must not go out.
        let delivery = deliver_direct_command(
            &direct("worker-a", "epoch-a"),
            CarrierPresence::live(false),
            &resync("session-b"),
        );
        assert!(matches!(
            delivery,
            Delivery::Refused(SendFault::SessionNotAdmitted { .. })
        ));
    }

    #[test]
    fn a_command_a_present_carrier_admits_is_encoded_from_the_effect() {
        let delivery = deliver_direct_command(
            &direct("worker-a", "epoch-a"),
            CarrierPresence::live(true),
            &resync("session-a"),
        );
        assert!(delivery.fault().is_none());
        let bytes = delivery.bytes().unwrap();
        let decoded = LocalTerminalClientFrame::decode_from_slice(bytes).unwrap();
        assert!(matches!(
            decoded.frame,
            Some(ClientFrame::TerminalResync(_))
        ));
    }

    #[test]
    fn a_sync_generation_is_refused_before_presence_is_read() {
        // Even when a carrier is reported present, a Sync token cannot be
        // presented BY one, so presence must not rescue it.
        let sync = TerminalToken::sync(1, "socket-a", "epoch-a", 1);
        assert_eq!(
            deliver_direct_command(&sync, CarrierPresence::live(true), &resync("session-a"))
                .fault(),
            Some(&SendFault::NoGeneration)
        );
    }
}
