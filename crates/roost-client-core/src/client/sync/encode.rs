//! `SyncCommand` → the `SyncClientFrame` bytes the coordinator's Sync socket
//! decodes (`crates/roost-coord/src/sync_ws/commands.rs`).
//!
//! Called by the host's pump for every `Effect::SendSync`, with the id of the
//! socket it is about to write to — v2 stamped `socketId` from the live link at
//! send time (`apps/web/src/store/sync-domain-state.ts:83-98`,
//! `apps/web/src/client/sync/sync-flow.ts:60-63`).

use roost_proto::__buffa::oneof::sync_client_frame::Command;
use roost_proto::buffa::{EnumValue, Message};
use roost_proto::{
    InputCommand, SyncClientFrame, SyncDomainReadyCommand, SyncDomainSubscriptionCommand,
    TerminalInputRouteClaim, TerminalResyncCommand, TerminalTransportProbe, TerminalViewCommand,
};

use crate::client::ui_state::LayoutApplyOutcome;
use crate::effect::SyncCommand;
use crate::sync::link::SyncDomain;
use crate::terminal::view::ViewIntent;

/// The frame bytes for one command on the socket `socket_id`.
///
/// Canonical protobuf, which the coordinator requires: it re-encodes what it
/// decoded and closes `1008` on any difference (`is_canonical_client_frame`).
/// Infallible because a Sync frame is bounded far below protobuf's 2 GiB
/// message limit — input batches are admitted in kilobytes.
pub fn encode_sync_command(command: &SyncCommand, socket_id: &str) -> Vec<u8> {
    let (ack_delivery_seq, command) = match command {
        SyncCommand::Ack { ack_delivery_seq } => (Some(*ack_delivery_seq), None),
        SyncCommand::Subscribe { domain, generation } => (
            None,
            Some(Command::DomainSubscribe(Box::new(subscription(
                *domain,
                *generation,
            )))),
        ),
        SyncCommand::Unsubscribe { domain, generation } => (
            None,
            Some(Command::DomainUnsubscribe(Box::new(subscription(
                *domain,
                *generation,
            )))),
        ),
        SyncCommand::DomainReady {
            domain,
            generation,
            snapshot_token,
        } => (
            None,
            Some(Command::DomainReady(Box::new(SyncDomainReadyCommand {
                domain: wire_domain(*domain),
                generation: *generation,
                snapshot_token: snapshot_token.clone(),
                ..Default::default()
            }))),
        ),
        SyncCommand::TerminalView {
            session_id,
            view_id,
            intent,
            revision,
            token,
        } => {
            // A parked or removed view is an INACTIVE lease with no geometry,
            // exactly v2's `changeIntent(view, false, 0, 0)`.
            let (cols, rows, active) = match intent {
                ViewIntent::Publish { cols, rows } => (*cols, *rows, true),
                ViewIntent::Park | ViewIntent::Unpublish => (0, 0, false),
            };
            (
                None,
                Some(Command::TerminalView(Box::new(TerminalViewCommand {
                    view_id: view_id.clone(),
                    session_id: session_id.clone(),
                    cols,
                    rows,
                    revision: *revision,
                    active,
                    domain_generation: token.domain_generation,
                    ..Default::default()
                }))),
            )
        }
        SyncCommand::TerminalResync {
            session_id,
            view_id,
            stream_id,
            grid_epoch,
            seq,
            token,
        } => (
            None,
            Some(Command::TerminalResync(Box::new(TerminalResyncCommand {
                view_id: view_id.clone(),
                session_id: session_id.clone(),
                stream_id: stream_id.clone(),
                grid_epoch: grid_epoch.clone(),
                seq: *seq,
                domain_generation: token.domain_generation,
                ..Default::default()
            }))),
        ),
        SyncCommand::TerminalInput {
            session_id,
            view_id,
            input_seq,
            bytes,
            input_route_epoch,
            token,
        } => (
            None,
            Some(Command::Input(Box::new(InputCommand {
                session_id: session_id.clone(),
                input_seq: *input_seq,
                data: bytes.clone(),
                domain_generation: token.domain_generation,
                view_id: view_id.clone(),
                input_route_epoch: input_route_epoch.clone(),
                ..Default::default()
            }))),
        ),
        SyncCommand::TerminalInputRouteClaim {
            session_id,
            request_id,
            revision,
            worker_epoch,
            token,
        } => (
            None,
            Some(Command::InputRouteClaim(Box::new(
                TerminalInputRouteClaim {
                    request_id: request_id.clone(),
                    session_id: session_id.clone(),
                    revision: *revision,
                    domain_generation: token.domain_generation,
                    worker_epoch: worker_epoch.clone(),
                    ..Default::default()
                },
            ))),
        ),
        SyncCommand::TerminalTransportProbe {
            request_id,
            worker_fp,
        } => (
            None,
            Some(Command::TerminalTransportProbe(Box::new(
                TerminalTransportProbe {
                    request_id: request_id.clone(),
                    worker_fp: worker_fp.clone(),
                    ..Default::default()
                },
            ))),
        ),
        SyncCommand::UiApplyLayoutResult(result) => (
            None,
            Some(Command::UiApplyLayoutResult(Box::new(
                roost_proto::UiApplyLayoutResult {
                    correlation_id: result.correlation_id.clone(),
                    outcome: EnumValue::from(match result.outcome {
                        LayoutApplyOutcome::Applied => roost_proto::UiApplyLayoutOutcome::Applied,
                        LayoutApplyOutcome::Rejected => roost_proto::UiApplyLayoutOutcome::Rejected,
                    }),
                    reason: result.reason.clone(),
                    ..Default::default()
                },
            ))),
        ),
    };
    // The coordinator's canonical order is FIELD-NUMBER order, and buffa writes
    // declaration order: `socket_id` (10) is declared after the oneof, so a
    // command numbered above it is written after it, as two concatenated parts
    // of one message. Without this every route claim closed the socket `1008`.
    let follows_socket_id = matches!(
        command,
        Some(Command::InputRouteClaim(_) | Command::TerminalTransportProbe(_))
    );
    if !follows_socket_id {
        return SyncClientFrame {
            ack_delivery_seq,
            socket_id: socket_id.to_string(),
            command,
            ..Default::default()
        }
        .encode_to_vec();
    }
    let mut bytes = SyncClientFrame {
        ack_delivery_seq,
        socket_id: socket_id.to_string(),
        ..Default::default()
    }
    .encode_to_vec();
    bytes.extend(
        SyncClientFrame {
            command,
            ..Default::default()
        }
        .encode_to_vec(),
    );
    bytes
}

fn subscription(domain: SyncDomain, generation: u64) -> SyncDomainSubscriptionCommand {
    SyncDomainSubscriptionCommand {
        domain: wire_domain(domain),
        generation,
        ..Default::default()
    }
}

fn wire_domain(domain: SyncDomain) -> EnumValue<roost_proto::SyncDomain> {
    EnumValue::from(domain.wire_value())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::token::TerminalToken;

    /// Every top-level field number in `bytes`, in the order written.
    fn field_numbers(mut bytes: &[u8]) -> Vec<u64> {
        fn varint(bytes: &mut &[u8]) -> u64 {
            let mut value = 0;
            for (idx, byte) in bytes.iter().enumerate() {
                value |= u64::from(byte & 0x7f) << (7 * idx);
                if byte & 0x80 == 0 {
                    *bytes = &bytes[idx + 1..];
                    return value;
                }
            }
            panic!("truncated varint");
        }
        let mut numbers = Vec::new();
        while !bytes.is_empty() {
            let key = varint(&mut bytes);
            numbers.push(key >> 3);
            match key & 7 {
                0 => {
                    varint(&mut bytes);
                }
                2 => {
                    let len = usize::try_from(varint(&mut bytes)).unwrap();
                    bytes = &bytes[len..];
                }
                other => panic!("unexpected wire type {other}"),
            }
        }
        numbers
    }

    #[test]
    fn every_command_is_written_in_field_number_order() {
        let token = TerminalToken::sync(3, "socket-a", "epoch-a", 7);
        let claim = SyncCommand::TerminalInputRouteClaim {
            session_id: "session-a".to_owned(),
            request_id: "claim-1".to_owned(),
            revision: 2,
            worker_epoch: "epoch-a".to_owned(),
            token: token.clone(),
        };
        assert_eq!(
            field_numbers(&encode_sync_command(&claim, "socket-a")),
            [10, 11]
        );
        let resync = SyncCommand::TerminalResync {
            session_id: "session-a".to_owned(),
            view_id: "view-a".to_owned(),
            stream_id: "stream-a".to_owned(),
            grid_epoch: "epoch-1".to_owned(),
            seq: 4,
            token,
        };
        assert_eq!(
            field_numbers(&encode_sync_command(&resync, "socket-a")),
            [8, 10]
        );
        let ack = SyncCommand::Ack {
            ack_delivery_seq: 9,
        };
        assert_eq!(
            field_numbers(&encode_sync_command(&ack, "socket-a")),
            [1, 10]
        );
    }
}
