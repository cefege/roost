//! Shared fixtures for the Sync decode suites: frames stamped the way the
//! coordinator stamps them, encoded with buffa, and a client taken to a link
//! whose every domain is subscribed and ready.
//!
//! The envelope stamping copies `crates/roost-coord/src/sync_ws/control_frames.rs`
//! (`control_frame`) and `retained_frame.rs` (`stamp_envelope`) rather than
//! depending on the coordinator. Used by `tests/sync_decode_*.rs`.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::effect::{Effect, RpcResult, SyncCommand};
use roost_client_core::event::ClientEvent;
use roost_client_core::sync::decode::{DecodeRefusal, SyncFrameMeta, decode_firehose};
use roost_client_core::{ClientCore, SyncDomain, SyncFrame};
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::buffa::Message;
use roost_proto::{FirehoseFrame, SyncDomainGeneration, SyncSubscribedFrame};

pub const TAB: &str = "tab-decode";
pub const SOCKET: &str = "sock-decode";
pub const EPOCH: &str = "epoch-decode";
pub const SNAPSHOT_TOKEN: &str = "terminal-snapshot-token";
/// Every domain's generation on the fixture link.
pub const DOMAIN_GENERATION: u64 = 3;
pub const SESSION: &str = "00000000-0000-4000-8000-00000000000a";
pub const WORKER_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// The proto enum for a client domain.
pub fn wire_domain(domain: SyncDomain) -> roost_proto::SyncDomain {
    match domain {
        SyncDomain::Terminal => roost_proto::SyncDomain::Terminal,
        SyncDomain::Workers => roost_proto::SyncDomain::Workers,
        SyncDomain::Workspaces => roost_proto::SyncDomain::Workspaces,
        SyncDomain::Tasks => roost_proto::SyncDomain::Tasks,
        SyncDomain::Mcp => roost_proto::SyncDomain::Mcp,
        SyncDomain::Pair => roost_proto::SyncDomain::Pair,
        SyncDomain::Audit => roost_proto::SyncDomain::Audit,
    }
}

/// A control frame, stamped as `control_frames::control_frame` stamps it.
pub fn control(arm: Frame) -> Vec<u8> {
    FirehoseFrame {
        delivery_seq: 0,
        domain: roost_proto::SyncDomain::Unspecified.into(),
        domain_generation: 0,
        frame: Some(arm),
        ..FirehoseFrame::default()
    }
    .encode_to_vec()
}

/// An application frame, stamped as `retained_frame::stamp_envelope` stamps it.
pub fn application(domain: SyncDomain, delivery_seq: u64, arm: Frame) -> Vec<u8> {
    stamped(wire_domain(domain), delivery_seq, DOMAIN_GENERATION, arm)
}

/// Any stamp at all, for the meta-rule cases.
pub fn stamped(
    domain: roost_proto::SyncDomain,
    delivery_seq: u64,
    domain_generation: u64,
    arm: Frame,
) -> Vec<u8> {
    FirehoseFrame {
        delivery_seq,
        domain: domain.into(),
        domain_generation,
        frame: Some(arm),
        ..FirehoseFrame::default()
    }
    .encode_to_vec()
}

/// Decode bytes delivered on `generation`, expecting a frame.
pub fn decoded(bytes: &[u8], generation: u64) -> ClientEvent {
    decode_firehose(bytes, SyncFrameMeta { generation }).expect("the frame decodes")
}

/// Decode bytes, expecting a refusal.
pub fn refused(bytes: &[u8]) -> DecodeRefusal {
    decode_firehose(bytes, SyncFrameMeta { generation: 1 }).expect_err("decode refuses the frame")
}

/// The frame inside a decoded event.
pub fn frame_of(event: &ClientEvent) -> &SyncFrame {
    match event {
        ClientEvent::SyncFrameReceived { frame, .. } => frame,
        other => panic!("expected a received frame, got {other:?}"),
    }
}

/// The `subscribed` barrier the coordinator sends: every domain, one
/// generation, subscribed.
pub fn subscribed_arm(socket_id: &str) -> Frame {
    Frame::Subscribed(Box::new(SyncSubscribedFrame {
        socket_id: socket_id.to_owned(),
        process_epoch: EPOCH.to_owned(),
        generations: SyncDomain::ALL
            .into_iter()
            .map(|domain| SyncDomainGeneration {
                domain: wire_domain(domain).into(),
                generation: DOMAIN_GENERATION,
                subscribed: true,
                ..SyncDomainGeneration::default()
            })
            .collect(),
        ..SyncSubscribedFrame::default()
    }))
}

/// A client whose link has every domain subscribed, hydrated and ready.
/// Returns the socket generation.
pub fn ready_core() -> (ClientCore, u64) {
    let mut core = ClientCore::in_memory(TAB);
    let generation = open_ready_link(&mut core);
    (core, generation)
}

/// Dial, open, decode the coordinator's `subscribed`, hydrate, take the
/// terminal snapshot token, and close every domain's snapshot/live gap.
pub fn open_ready_link(core: &mut ClientCore) -> u64 {
    let generation = match core.handle(ClientEvent::DialRequested).as_slice() {
        [Effect::DialSync { generation, .. }] => *generation,
        other => panic!("expected exactly one dial, got {other:?}"),
    };
    core.handle(ClientEvent::SyncLinkOpened {
        generation,
        socket_id: SOCKET.to_owned(),
        process_epoch: EPOCH.to_owned(),
    });
    core.handle(decoded(&control(subscribed_arm(SOCKET)), generation));
    core.handle(ClientEvent::HydrationCompleted { generation });
    core.handle(ClientEvent::RpcResultReceived(RpcResult::SessionsList {
        call_id: 1,
        sessions: Default::default(),
        terminal_snapshot_token: Some(SNAPSHOT_TOKEN.to_owned()),
    }));
    for domain in SyncDomain::ALL {
        core.handle(ClientEvent::SyncFrameReceived {
            generation,
            delivery_seq: 0,
            frame: SyncFrame::DomainReady {
                domain,
                generation: DOMAIN_GENERATION,
                snapshot_token: (domain == SyncDomain::Terminal).then(|| SNAPSHOT_TOKEN.to_owned()),
            },
        });
        assert!(
            core.store().sync.domain_is_ready(domain),
            "{domain:?} is ready"
        );
    }
    generation
}

/// The recovery cursor, read as the `since` the next dial sends. Beginning a
/// dial leaves the current link alone.
pub fn cursor(core: &mut ClientCore) -> u64 {
    match core.handle(ClientEvent::DialRequested).as_slice() {
        [Effect::DialSync { dial, .. }] => dial.since,
        other => panic!("expected exactly one dial, got {other:?}"),
    }
}

/// Decode on `generation` and apply.
pub fn deliver(core: &mut ClientCore, generation: u64, bytes: &[u8]) -> Vec<Effect> {
    core.handle(decoded(bytes, generation))
}

/// The acknowledged delivery sequences among `effects`.
pub fn acked(effects: &[Effect]) -> Vec<u64> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::SendSync(SyncCommand::Ack { ack_delivery_seq }) => Some(*ack_delivery_seq),
            _ => None,
        })
        .collect()
}

/// Whether `effects` close the link of `generation`.
pub fn closes(effects: &[Effect], generation: u64) -> bool {
    effects.iter().any(|effect| {
        matches!(effect, Effect::CloseSyncLink { generation: closed, .. } if *closed == generation)
    })
}
