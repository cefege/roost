//! Fixtures for the UI command and UI state report tests: a store holding a
//! few sessions in two folders, and the wire frames the Sync fold queues.
//!
//! Folder keys come from `ExactWorkerPaths`, so a session's bucket is
//! `<MACHINE>::<cwd>` and a test can name it without a path codec.
#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::ClientCore;
use roost_client_core::store::{
    ChannelId, Session, SessionId, SessionKind, SessionMap, SessionStatus, WorkerFp,
};
use roost_proto::__buffa::oneof::ui_command::Command as WireCommand;
use roost_proto::buffa::MessageField;

/// A machine fingerprint.
pub const MACHINE: &str = "00000000000000000000000000000000000000000000000000000000000000ff";
/// The tab the client under test presents.
pub const OWN_TAB: &str = "tab-own";
/// Two sessions in `/work`, oldest first, and one in `/other`.
pub const ALPHA: &str = "00000000-0000-4000-8000-000000000001";
pub const BETA: &str = "00000000-0000-4000-8000-000000000002";
pub const GAMMA: &str = "00000000-0000-4000-8000-000000000003";
/// A closed session in `/work`.
pub const CLOSED: &str = "00000000-0000-4000-8000-000000000004";
/// An id no store holds.
pub const UNKNOWN: &str = "00000000-0000-4000-8000-0000000000ff";
/// A client-minted optimistic spawn id.
pub const PENDING: &str = "00000000-0000-4000-8000-0000000000aa";

/// The bucket of `/work`.
pub fn work_folder() -> String {
    format!("{MACHINE}::/work")
}

/// The bucket of `/other`.
pub fn other_folder() -> String {
    format!("{MACHINE}::/other")
}

pub fn session(id: &str, cwd: &str, created_at: i64, status: SessionStatus) -> Session {
    Session {
        id: SessionId::try_from(id.to_owned()).expect("a uuid"),
        worker_fp: WorkerFp::try_from(MACHINE.to_owned()).expect("a fingerprint"),
        channel: ChannelId::try_from(7_i64).expect("a channel"),
        kind: SessionKind::Shell,
        cwd: cwd.to_owned(),
        spawn_cwd: Some(cwd.to_owned()),
        workspace_id: None,
        status,
        created_at,
        closed_at: None,
        custom_title: None,
        git_branch: None,
        git_remote: None,
        pr_number: None,
        pr_state: None,
        pr_checks: None,
        pr_url: None,
        ports: None,
    }
}

/// A client whose store holds ALPHA, BETA (`/work`), GAMMA (`/other`) and
/// the closed CLOSED (`/work`).
pub fn seeded_core() -> ClientCore {
    let mut core = ClientCore::in_memory(OWN_TAB);
    let mut map = SessionMap::new();
    for row in [
        session(ALPHA, "/work", 100, SessionStatus::Open),
        session(BETA, "/work", 200, SessionStatus::Open),
        session(GAMMA, "/other", 300, SessionStatus::Open),
        session(CLOSED, "/work", 50, SessionStatus::Closed),
    ] {
        map.insert(row.id.clone(), row);
    }
    core.store_mut().sessions.apply_snapshot(map);
    core
}

/// A queued frame carrying `command`, addressed as given.
pub fn frame(
    target_tab_id: &str,
    target_socket_id: &str,
    correlation_id: &str,
    command: Option<WireCommand>,
) -> roost_proto::UiCommandFrame {
    roost_proto::UiCommandFrame {
        target_tab_id: target_tab_id.to_owned(),
        target_socket_id: target_socket_id.to_owned(),
        correlation_id: correlation_id.to_owned(),
        command: MessageField::some(roost_proto::UiCommand {
            command,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// A legacy frame for `target_tab_id`.
pub fn legacy(target_tab_id: &str, command: WireCommand) -> roost_proto::UiCommandFrame {
    frame(target_tab_id, "", "", Some(command))
}

pub fn navigate(path: &str) -> WireCommand {
    WireCommand::Navigate(Box::new(roost_proto::UiNavigate {
        path: path.to_owned(),
        ..Default::default()
    }))
}

pub fn select_tab(session_id: &str) -> WireCommand {
    WireCommand::SelectTab(Box::new(roost_proto::UiSelectTab {
        session_id: session_id.to_owned(),
        ..Default::default()
    }))
}

pub fn focus_pane(session_id: &str) -> WireCommand {
    WireCommand::FocusPane(Box::new(roost_proto::UiFocusPane {
        session_id: session_id.to_owned(),
        ..Default::default()
    }))
}

pub fn close_tab(session_id: &str) -> WireCommand {
    WireCommand::CloseTab(Box::new(roost_proto::UiCloseTab {
        session_id: session_id.to_owned(),
        ..Default::default()
    }))
}

pub fn spotlight(session_id: &str, off: bool) -> WireCommand {
    WireCommand::Spotlight(Box::new(roost_proto::UiSpotlight {
        session_id: session_id.to_owned(),
        off,
        ..Default::default()
    }))
}

pub fn place_split(session_id: &str, anchor: &str) -> WireCommand {
    WireCommand::PlaceSplit(Box::new(roost_proto::UiPlaceSplit {
        session_id: session_id.to_owned(),
        anchor_session_id: anchor.to_owned(),
        dir: "row".to_owned(),
        ..Default::default()
    }))
}

pub fn move_tab(session_id: &str, dest: &str) -> WireCommand {
    WireCommand::MoveTab(Box::new(roost_proto::UiMoveTab {
        session_id: session_id.to_owned(),
        dest_session_id: dest.to_owned(),
        ..Default::default()
    }))
}

pub fn arrange(preset: &str) -> WireCommand {
    WireCommand::Arrange(Box::new(roost_proto::UiArrange {
        preset: preset.to_owned(),
        ..Default::default()
    }))
}

pub fn apply_layout(document: Option<roost_proto::LayoutDocumentV1>) -> WireCommand {
    WireCommand::ApplyLayout(Box::new(roost_proto::UiApplyLayout {
        document: document.map_or(MessageField::none(), MessageField::some),
        ..Default::default()
    }))
}
