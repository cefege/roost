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
    TerminalResyncCommand, TerminalViewCommand,
};

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
    };
    SyncClientFrame {
        ack_delivery_seq,
        socket_id: socket_id.to_string(),
        command,
        ..Default::default()
    }
    .encode_to_vec()
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
