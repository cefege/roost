//! Sync socket bytes → one `ClientEvent`: protobuf decode, the v2 meta rule, and
//! the proto→wire conversion of every `FirehoseFrame` arm.
//!
//! Called by the host's Sync pump on every binary message; the `Ok` event or a
//! `ClientEvent::SyncFrameRefused` built from the `Err` goes to
//! `ClientCore::handle`. Depends on `roost_proto` for the wire messages,
//! `roost_protocol` for the shared wire shapes, and `client::rpc::codec::wire_rows`
//! for the rows a delta and a hydration list share. Ported from
//! `apps/web/src/store/sync-inbound.ts` (`handleV2Control`,
//! `dispatchV2Application`), `apps/web/src/store/sync-frame.ts:102-371` and
//! `apps/web/src/client/sync/sync-proto-adapters.ts`.
//!
//! A refusal is FATAL for the link, never for one frame: every refusal here is a
//! case where v2's `_consumeSyncFrame` reaches `_closeFailedSyncLink` — a throw,
//! or a dispatch that returned `false` and so came back "unapplied"
//! (`apps/web/src/client/sync/sync-flow.ts:52-55`, `sync-inbound.ts:74-83`).

mod arms;
mod control_arms;
mod registry_arms;
mod terminal_arms;

use std::fmt;

use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::FirehoseFrame;
use roost_proto::buffa::Message;

pub use self::arms::{ArmLane, FIREHOSE_ARMS, FirehoseArm, arm_of};
pub(crate) use self::control_arms::{input_route_result_of, transport_probe_result_of};
use crate::event::ClientEvent;
use crate::sync::inbound::SyncFrame;
use crate::sync::link::SyncDomain;

/// What the host knows about the bytes besides the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncFrameMeta {
    /// The host's socket generation that delivered the bytes.
    pub generation: u64,
}

/// Why bytes from the Sync socket are not a frame this client may apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeRefusal {
    /// The bytes are not a `FirehoseFrame`.
    Undecodable {
        /// The protobuf decoder's complaint.
        reason: String,
    },
    /// A `FirehoseFrame` whose oneof is empty, including an arm this build's
    /// proto does not know. v2: `!frame.frame.case` is "unapplied".
    NoFrame,
    /// A control arm stamped with a sequence, domain or domain generation
    /// (v2 `handleV2Control`: "sequenced v2 control").
    SequencedControl {
        /// The arm's proto field name.
        arm: &'static str,
        /// The stamped sequence.
        delivery_seq: u64,
        /// The stamped domain's wire value.
        domain: i32,
        /// The stamped domain generation.
        domain_generation: u64,
    },
    /// An application arm without a sequence or without a known domain
    /// (v2 `dispatchV2Application`: "malformed v2 application frame"; with
    /// `delivery_seq = 0` v2 routes it to `handleV2Control`, whose default
    /// case closes the link as an "unknown v2 control").
    UnsequencedApplication {
        /// The arm's proto field name.
        arm: &'static str,
        /// The stamped sequence.
        delivery_seq: u64,
        /// The stamped domain's wire value.
        domain: i32,
    },
    /// The arm's payload failed a conversion v2 treats as fatal.
    MalformedArm {
        /// The arm's proto field name.
        arm: &'static str,
        /// What was wrong with it.
        reason: String,
    },
}

impl fmt::Display for DecodeRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Undecodable { reason } => write!(formatter, "undecodable sync frame: {reason}"),
            Self::NoFrame => formatter.write_str("sync frame carries no known arm"),
            Self::SequencedControl {
                arm,
                delivery_seq,
                domain,
                domain_generation,
            } => write!(
                formatter,
                "sequenced v2 control {arm}: delivery_seq={delivery_seq} domain={domain} \
                 domain_generation={domain_generation}"
            ),
            Self::UnsequencedApplication {
                arm,
                delivery_seq,
                domain,
            } => write!(
                formatter,
                "malformed v2 application frame {arm}: delivery_seq={delivery_seq} domain={domain}"
            ),
            Self::MalformedArm { arm, reason } => write!(formatter, "malformed {arm}: {reason}"),
        }
    }
}

impl std::error::Error for DecodeRefusal {}

/// Decode one binary Sync message into the event that applies it.
pub fn decode_firehose(bytes: &[u8], meta: SyncFrameMeta) -> Result<ClientEvent, DecodeRefusal> {
    let envelope =
        FirehoseFrame::decode_from_slice(bytes).map_err(|error| DecodeRefusal::Undecodable {
            reason: error.to_string(),
        })?;
    let FirehoseFrame {
        delivery_seq,
        domain_generation,
        domain,
        frame,
        ..
    } = envelope;
    let frame = frame.ok_or(DecodeRefusal::NoFrame)?;
    let arm = arm_of(&frame);
    check_meta(arm, delivery_seq, domain.to_i32(), domain_generation)?;
    let frame =
        map_arm(frame, domain_generation).map_err(|reason| DecodeRefusal::MalformedArm {
            arm: arm.name,
            reason,
        })?;
    Ok(ClientEvent::SyncFrameReceived {
        generation: meta.generation,
        delivery_seq,
        frame,
    })
}

/// The meta rule: a control is unstamped, an application frame is sequenced
/// and names a domain this client knows.
fn check_meta(
    arm: FirehoseArm,
    delivery_seq: u64,
    domain: i32,
    domain_generation: u64,
) -> Result<(), DecodeRefusal> {
    match arm.lane {
        ArmLane::Control if delivery_seq != 0 || domain != 0 || domain_generation != 0 => {
            Err(DecodeRefusal::SequencedControl {
                arm: arm.name,
                delivery_seq,
                domain,
                domain_generation,
            })
        }
        ArmLane::Application(_) if delivery_seq == 0 || known_domain(domain).is_none() => {
            Err(DecodeRefusal::UnsequencedApplication {
                arm: arm.name,
                delivery_seq,
                domain,
            })
        }
        ArmLane::Control | ArmLane::Application(_) => Ok(()),
    }
}

/// The client domain for a wire value, or `None` for `UNSPECIFIED`, a retired
/// value, or one this build does not know.
pub(crate) fn known_domain(wire_value: i32) -> Option<SyncDomain> {
    SyncDomain::ALL
        .into_iter()
        .find(|domain| domain.wire_value() == wire_value)
}

/// The one exhaustive match over the proto oneof: a new arm in `sync.proto` is a
/// compile error here, not a silently dropped frame.
fn map_arm(frame: Frame, domain_generation: u64) -> Result<SyncFrame, String> {
    use self::control_arms as control;
    use self::registry_arms as registry;
    use self::terminal_arms as terminal;
    match frame {
        Frame::Subscribed(value) => control::subscribed(*value),
        Frame::DomainReset(value) => control::domain_reset(*value),
        Frame::InputAccepted(value) => Ok(control::input_accepted(*value)),
        Frame::InputRejected(value) => Ok(control::input_rejected(*value)),
        Frame::InputAmbiguous(value) => Ok(control::input_ambiguous(*value)),
        Frame::InputRouteResult(value) => Ok(control::input_route_result(*value)),
        Frame::TerminalTransportProbeResult(value) => Ok(control::transport_probe_result(*value)),
        Frame::UiState(_) => Ok(SyncFrame::UiState),
        Frame::UiCommand(value) => Ok(SyncFrame::UiCommand { command: *value }),
        Frame::Keepalive(_) => Ok(SyncFrame::Keepalive),
        Frame::CoordinatorRelocation(value) => Ok(control::coordinator_relocation(*value)),
        Frame::Sessions(value) => terminal::sessions_json(&value.payload_json),
        Frame::SessionEvent(value) => terminal::session_event(&value),
        Frame::SessionPresence(value) => terminal::session_presence(*value),
        Frame::CellGrid(value) => Ok(SyncFrame::CellGrid {
            session_id: value.session_id.clone(),
            frame: *value,
        }),
        Frame::CellGridChunk(value) => Ok(terminal::cell_grid_chunk(*value)),
        Frame::TerminalViewState(value) => Ok(terminal::view_state(*value, domain_generation)),
        Frame::TerminalTitle(value) => Ok(SyncFrame::TerminalTitle {
            session_id: value.session_id,
            title: value.title,
        }),
        Frame::LastActivity(value) => Ok(terminal::last_activity(*value)),
        Frame::AgentStatus(value) => Ok(terminal::agent_status(*value)),
        Frame::AuditRow(value) => Ok(registry::audit_row(*value)),
        Frame::WorkspaceDelta(value) => registry::workspace_delta(*value),
        Frame::TaskDelta(value) => registry::task_delta(*value),
        Frame::McpMsg(value) => registry::mcp_message(*value),
        Frame::WorkerPresence(value) => registry::worker_presence(*value),
        Frame::WorkerRoutable(value) => registry::worker_routable(*value),
        Frame::PairRequestDelta(value) => registry::pair_request_delta(*value),
    }
}
