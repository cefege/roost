//! Every `SyncCommand` encodes to the `SyncClientFrame` the coordinator's Sync
//! socket decodes (`crates/roost-coord/src/sync_ws/commands.rs`): the right
//! oneof arm, the host's socket id on every frame, and canonical bytes — the
//! coordinator closes `1008` on a frame that does not re-encode identically.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::SyncCommand;
use roost_client_core::client::sync::encode_sync_command;
use roost_client_core::sync::link::SyncDomain;
use roost_client_core::terminal::{TerminalToken, ViewIntent};
use roost_proto::__buffa::oneof::sync_client_frame::Command;
use roost_proto::buffa::Message;
use roost_proto::{SyncClientFrame, SyncDomain as PbDomain};

const SOCKET: &str = "socket-7";
const SESSION: &str = "00000000-0000-4000-8000-00000000000a";

fn token(domain_generation: u64) -> TerminalToken {
    TerminalToken::sync(3, SOCKET, "epoch-1", domain_generation)
}

/// Decode as the coordinator does, and hold the bytes to its canonical rule.
fn sent(command: &SyncCommand) -> SyncClientFrame {
    let bytes = encode_sync_command(command, SOCKET);
    let frame = SyncClientFrame::decode_from_slice(&bytes).unwrap();
    assert_eq!(
        frame.encode_to_vec(),
        bytes,
        "the coordinator refuses non-canonical frames"
    );
    assert_eq!(
        frame.socket_id, SOCKET,
        "every frame names the socket it is sent on"
    );
    frame
}

#[test]
fn an_ack_is_a_bare_cumulative_sequence() {
    let frame = sent(&SyncCommand::Ack {
        ack_delivery_seq: 41,
    });
    assert_eq!(frame.ack_delivery_seq, Some(41));
    assert!(frame.command.is_none(), "v2 sends an ACK with no command");
}

#[test]
fn a_subscribe_and_an_unsubscribe_name_the_domain_and_its_generation() {
    let subscribe = sent(&SyncCommand::Subscribe {
        domain: SyncDomain::Workers,
        generation: 4,
    });
    let Some(Command::DomainSubscribe(command)) = subscribe.command else {
        panic!(
            "a subscribe must go out as domain_subscribe, got {:?}",
            subscribe.command
        );
    };
    assert_eq!(
        command.domain.as_known(),
        Some(PbDomain::SYNC_DOMAIN_WORKERS)
    );
    assert_eq!(command.generation, 4);
    assert_eq!(subscribe.ack_delivery_seq, None);

    let unsubscribe = sent(&SyncCommand::Unsubscribe {
        domain: SyncDomain::Audit,
        generation: 2,
    });
    let Some(Command::DomainUnsubscribe(command)) = unsubscribe.command else {
        panic!(
            "an unsubscribe must go out as domain_unsubscribe, got {:?}",
            unsubscribe.command
        );
    };
    assert_eq!(command.domain.as_known(), Some(PbDomain::SYNC_DOMAIN_AUDIT));
    assert_eq!(command.generation, 2);
}

#[test]
fn a_domain_ready_presents_the_generation_and_the_snapshot_token() {
    let frame = sent(&SyncCommand::DomainReady {
        domain: SyncDomain::Terminal,
        generation: 9,
        snapshot_token: Some("snapshot-1".to_owned()),
    });
    let Some(Command::DomainReady(command)) = frame.command else {
        panic!(
            "domain_ready must go out as domain_ready, got {:?}",
            frame.command
        );
    };
    assert_eq!(
        command.domain.as_known(),
        Some(PbDomain::SYNC_DOMAIN_TERMINAL)
    );
    assert_eq!(command.generation, 9);
    assert_eq!(command.snapshot_token.as_deref(), Some("snapshot-1"));
}

#[test]
fn a_published_view_is_an_active_lease_at_its_geometry() {
    let frame = sent(&SyncCommand::TerminalView {
        session_id: SESSION.to_owned(),
        view_id: "view-1".to_owned(),
        intent: ViewIntent::Publish {
            cols: 120,
            rows: 40,
        },
        revision: 3,
        token: token(6),
    });
    let Some(Command::TerminalView(view)) = frame.command else {
        panic!(
            "a view must go out as terminal_view, got {:?}",
            frame.command
        );
    };
    assert_eq!(
        (view.session_id.as_str(), view.view_id.as_str()),
        (SESSION, "view-1")
    );
    assert_eq!((view.cols, view.rows, view.active), (120, 40, true));
    assert_eq!(view.revision, 3);
    assert_eq!(view.domain_generation, 6);
}

#[test]
fn a_parked_or_removed_view_is_an_inactive_lease_with_no_geometry() {
    // v2 `changeIntent(view, false, 0, 0)` for both setInactive and dispose.
    for intent in [ViewIntent::Park, ViewIntent::Unpublish] {
        let frame = sent(&SyncCommand::TerminalView {
            session_id: SESSION.to_owned(),
            view_id: "view-1".to_owned(),
            intent,
            revision: 4,
            token: token(6),
        });
        let Some(Command::TerminalView(view)) = frame.command else {
            panic!("{intent:?} must go out as terminal_view");
        };
        assert_eq!(
            (view.cols, view.rows, view.active),
            (0, 0, false),
            "{intent:?}"
        );
        assert_eq!(view.revision, 4);
    }
}

#[test]
fn a_resync_names_the_stream_position_it_is_a_baseline_of() {
    let frame = sent(&SyncCommand::TerminalResync {
        session_id: SESSION.to_owned(),
        view_id: "view-1".to_owned(),
        stream_id: "stream-1".to_owned(),
        grid_epoch: "g-2".to_owned(),
        seq: 17,
        token: token(5),
    });
    let Some(Command::TerminalResync(resync)) = frame.command else {
        panic!(
            "a resync must go out as terminal_resync, got {:?}",
            frame.command
        );
    };
    assert_eq!(
        (resync.session_id.as_str(), resync.view_id.as_str()),
        (SESSION, "view-1")
    );
    assert_eq!(
        (resync.stream_id.as_str(), resync.grid_epoch.as_str()),
        ("stream-1", "g-2")
    );
    assert_eq!(resync.seq, 17);
    assert_eq!(resync.domain_generation, 5);
}

#[test]
fn an_input_batch_carries_its_bytes_sequence_route_and_fence() {
    let frame = sent(&SyncCommand::TerminalInput {
        session_id: SESSION.to_owned(),
        view_id: Some("view-1".to_owned()),
        input_seq: 12,
        bytes: b"ls\r".to_vec(),
        input_route_epoch: "route-3".to_owned(),
        token: token(8),
    });
    let Some(Command::Input(input)) = frame.command else {
        panic!("input must go out as input, got {:?}", frame.command);
    };
    assert_eq!(input.session_id, SESSION);
    assert_eq!(input.view_id.as_deref(), Some("view-1"));
    assert_eq!(input.input_seq, 12);
    assert_eq!(input.data, b"ls\r");
    assert_eq!(input.input_route_epoch, "route-3");
    assert_eq!(
        input.domain_generation, 8,
        "the coordinator fences input on it"
    );
}
