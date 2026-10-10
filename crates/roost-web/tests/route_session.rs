//! Route → session resolution and the stable terminal URL: every terminal route
//! shape resolves to the session MainPane renders; non-terminal routes resolve
//! to nothing; `/t/` hrefs round-trip; the close/bounce landing is the newest
//! open sibling in the folder, else home. Ports
//! `apps/web/tests/{activeSessionForPath,terminalHref}.test.ts` and the sibling
//! cases of `deadRouteSafetyNet.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::ClientCore;
use roost_client_core::store::{
    ChannelId, Session, SessionId, SessionKind, SessionMap, SessionStatus, WorkerFp, WorkspaceId,
};
use roost_protocol::wire::agent_chat::{AgentRunState, ConversationSummary};
use roost_web::app::{ServedSurface, Surface, surface_for};
use roost_web::platform::worker_paths::BrowserWorkerPaths;
use roost_web::route_session::{
    active_deck_tab_for_route, active_open_session_for_route, active_session_for_path,
    sibling_or_home_href,
};
use roost_web::routes::Route;
use roost_web::terminal_href::{decode_folder_path, encode_folder_path, terminal_href};

const ID: &str = "00000000-0000-4000-8000-000000000001";
const SECOND: &str = "00000000-0000-4000-8000-000000000002";
const FOLDER: &str = "/Users/you/roost";

fn fp() -> String {
    "aa".repeat(32)
}

fn session(id: &str, channel: i64, created_at: i64) -> Session {
    Session {
        id: SessionId::try_from(id.to_owned()).unwrap(),
        worker_fp: WorkerFp::try_from(fp()).unwrap(),
        channel: ChannelId::try_from(channel).unwrap(),
        kind: SessionKind::Shell,
        cwd: FOLDER.into(),
        spawn_cwd: Some(FOLDER.into()),
        workspace_id: Some(
            WorkspaceId::try_from("00000000-0000-4000-8000-0000000000aa".to_owned()).unwrap(),
        ),
        status: SessionStatus::Open,
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

fn core_with(rows: Vec<Session>) -> ClientCore {
    let mut core = ClientCore::in_memory("tab-route");
    let mut map = SessionMap::new();
    for row in rows {
        map.insert(row.id.clone(), row);
    }
    core.store_mut().sessions.apply_snapshot(map);
    core
}

fn resolved(core: &ClientCore, path: &str) -> Option<String> {
    active_session_for_path(core.store(), &BrowserWorkerPaths, path)
        .map(|session| session.id.as_str().to_owned())
}

#[test]
fn a_session_route_resolves_by_id() {
    let core = core_with(vec![session(ID, 1, 1000)]);
    assert_eq!(resolved(&core, &format!("/s/{ID}")).as_deref(), Some(ID));
}

#[test]
fn a_folder_route_resolves_by_machine_and_spawn_folder() {
    let core = core_with(vec![session(ID, 1, 1000)]);
    let path = format!("/t/{}/{}", fp(), encode_folder_path(None, FOLDER));
    assert_eq!(resolved(&core, &path).as_deref(), Some(ID));
}

#[test]
fn a_workspace_route_resolves_to_its_newest_open_session() {
    let core = core_with(vec![session(ID, 1, 1000), session(SECOND, 2, 2000)]);
    assert_eq!(
        resolved(&core, "/w/00000000-0000-4000-8000-0000000000aa").as_deref(),
        Some(SECOND)
    );
}

#[test]
fn a_legacy_channel_route_resolves_the_channel_it_names() {
    let core = core_with(vec![session(ID, 1, 2000), session(SECOND, 2, 1000)]);
    assert_eq!(
        resolved(&core, "/w/00000000-0000-4000-8000-0000000000aa/t/2").as_deref(),
        Some(SECOND)
    );
    assert_eq!(
        resolved(&core, "/w/00000000-0000-4000-8000-0000000000aa/t/x"),
        None
    );
}

#[test]
fn non_terminal_routes_and_an_unknown_workspace_resolve_to_nothing() {
    let core = core_with(vec![session(ID, 1, 1000)]);
    for path in ["/settings/machines", "/search", "/", "/w/nope"] {
        assert_eq!(resolved(&core, path), None, "{path}");
    }
}

#[test]
fn a_closed_session_resolves_but_is_not_the_open_one_the_deck_renders() {
    let mut closed = session(ID, 1, 1000);
    closed.status = SessionStatus::Closed;
    let core = core_with(vec![closed]);
    let route = Route::parse(&format!("/s/{ID}"));
    assert_eq!(resolved(&core, &format!("/s/{ID}")).as_deref(), Some(ID));
    assert!(active_open_session_for_route(core.store(), &BrowserWorkerPaths, &route).is_none());
}

#[test]
fn every_terminal_file_and_search_route_is_one_main_pane_surface() {
    for path in [
        "/s/abc",
        "/t/aa/src",
        "/w/ws1",
        "/w/ws1/t/3",
        "/file/aa/etc/hosts",
        "/a/abc",
        "/search",
    ] {
        assert_eq!(
            surface_for(&Route::parse(path)),
            Surface::Served(ServedSurface::MainPane),
            "{path}"
        );
    }
}

#[test]
fn an_agent_route_selects_its_agent_deck_tab() {
    let mut core = core_with(Vec::new());
    core.store_mut().agent_chat.conversations.insert(
        "abc".to_owned(),
        ConversationSummary {
            id: "abc".to_owned(),
            title: "Agent".to_owned(),
            worker_fp: fp(),
            worker_label: "dev".to_owned(),
            cwd: FOLDER.to_owned(),
            model: None,
            thinking_level: None,
            run_state: AgentRunState::Idle,
            error: None,
            created_ms: 1,
            updated_ms: 1,
        },
    );
    assert_eq!(
        active_deck_tab_for_route(core.store(), &BrowserWorkerPaths, &Route::parse("/a/abc"))
            .as_deref(),
        Some("agent:abc")
    );
}

#[test]
fn folder_paths_round_trip_through_the_route_codec() {
    for folder in [
        "/Users/you/roost",
        "/Users/you/My Folder",
        "/Users/you/café/apps",
        "/",
    ] {
        let encoded = encode_folder_path(None, folder);
        assert_eq!(
            decode_folder_path(None, &encoded).as_deref(),
            Some(folder),
            "{folder}"
        );
    }
    assert_eq!(
        encode_folder_path(None, "/Users/you/My Folder"),
        "Users/you/My%20Folder"
    );
}

#[test]
fn windows_drive_and_unc_folders_use_tagged_reversible_routes() {
    let drive = "C:/Users/Ada/My Folder";
    let unc = "//fileserver/team/Build Artifacts";
    assert_eq!(
        encode_folder_path(None, drive),
        "~drive/C/Users/Ada/My%20Folder"
    );
    assert_eq!(
        decode_folder_path(None, &encode_folder_path(None, drive)).as_deref(),
        Some(drive)
    );
    assert_eq!(
        encode_folder_path(None, unc),
        "~unc/fileserver/team/Build%20Artifacts"
    );
    assert_eq!(
        decode_folder_path(None, &encode_folder_path(None, unc)).as_deref(),
        Some(unc)
    );
}

#[test]
fn terminal_href_builds_the_folder_url_or_falls_back_to_the_session_url() {
    let core = core_with(vec![]);
    assert_eq!(
        terminal_href(core.store(), &session(ID, 1, 1000)),
        format!("/t/{}/Users/you/roost", fp())
    );
    let mut windows = session(ID, 1, 1000);
    windows.spawn_cwd = Some("D:/src/roost".into());
    assert_eq!(
        terminal_href(core.store(), &windows),
        format!("/t/{}/~drive/D/src/roost", fp())
    );
    let mut legacy = session(ID, 1, 1000);
    legacy.spawn_cwd = None;
    assert_eq!(terminal_href(core.store(), &legacy), format!("/s/{ID}"));
}

#[test]
fn a_gone_session_lands_on_its_newest_open_sibling_else_home() {
    let viewed = session(ID, 1, 1000);
    let core = core_with(vec![viewed.clone(), session(SECOND, 2, 2000)]);
    assert_eq!(
        sibling_or_home_href(core.store(), &BrowserWorkerPaths, &viewed),
        format!("/s/{SECOND}")
    );
    let alone = core_with(vec![viewed.clone()]);
    assert_eq!(
        sibling_or_home_href(alone.store(), &BrowserWorkerPaths, &viewed),
        "/"
    );
}
