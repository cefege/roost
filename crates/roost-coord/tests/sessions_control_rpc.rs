//! `SessionsAttach`, `SessionsKill`, `SessionsRename`, `SessionsCursorPos` and
//! `SessionsAssignWorkspace` against a recording worker socket and a real
//! database: what reaches the worker, what is durably appended, and what the
//! buses carry.
//!
//! Ports the kill and cursor-presence cases of `apps/coord/tests/coord-bidi.test.ts`
//! and the handler behaviour of `apps/coord/src/sessions/handlers-sessions.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod sessions_support;

use std::sync::{Arc, Mutex};

use connectrpc::ErrorCode;
use roost_coord::coord_core::seams::{LiveChannel, WorkerRouteIndex};
use roost_coord::sessions::assign_workspace::handle_sessions_assign_workspace;
use roost_coord::sessions::cursor_pos::handle_sessions_cursor_pos;
use roost_coord::sessions::rpc_sessions::{
    handle_sessions_attach, handle_sessions_kill, handle_sessions_rename,
};
use roost_proto::{
    SessionsAssignWorkspaceRequest, SessionsAttachRequest, SessionsCursorPosRequest,
    SessionsKillRequest, SessionsRenameRequest,
};
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::{ChannelId, SessionId, WorkerFp, WorkspaceDelta};
use serde_json::{Value, json};
use sessions_support::{BROWSER_FP, SessionsHarness, TAB, WORKER_FP, session, workspace};

fn kill(session_id: &str, force: bool) -> SessionsKillRequest {
    SessionsKillRequest {
        session_id: session_id.to_owned(),
        force,
        ..Default::default()
    }
}

// v2 coord-bidi: "SessionsKill on unknown session → accepted falsy (idempotent)".
#[tokio::test]
async fn killing_an_unknown_session_is_not_accepted() {
    let harness = SessionsHarness::new("kill-unknown").await;
    harness.connect_worker();
    let caller = harness.device(None);
    let nil = "00000000-0000-0000-0000-000000000000";
    let answer = handle_sessions_kill(&harness.core, &caller, kill(nil, true))
        .await
        .unwrap();
    assert!(!answer.body.accepted);
    assert!(harness.commands().is_empty());
}

#[tokio::test]
async fn a_live_kill_is_relayed_and_an_offline_one_needs_force() {
    let harness = SessionsHarness::new("kill").await;
    let caller = harness.device(Some(TAB));
    let sid = session("1");
    harness.seed_session(&sid, 1).await;

    // Offline and not forced: a transient disconnect must never remove a session.
    let refused = handle_sessions_kill(&harness.core, &caller, kill(&sid, false))
        .await
        .unwrap();
    assert!(!refused.body.accepted);
    assert_eq!(
        harness
            .scalar(&format!("SELECT count(*) FROM sessions WHERE id = '{sid}'"))
            .await,
        1
    );

    // Forced: the durable `closed` tombstone removes the row.
    let forced = handle_sessions_kill(&harness.core, &caller, kill(&sid, true))
        .await
        .unwrap();
    assert!(forced.body.accepted);
    assert_eq!(
        harness
            .scalar(&format!("SELECT count(*) FROM sessions WHERE id = '{sid}'"))
            .await,
        0
    );
    let tombstones =
        format!("SELECT count(*) FROM events WHERE session_id = '{sid}' AND kind = 'closed'");
    assert_eq!(harness.scalar(&tombstones).await, 1);

    // Live: the kill goes to the worker under the bare device fingerprint.
    harness.connect_worker();
    let live = session("2");
    harness.seed_session(&live, 2).await;
    let relayed = handle_sessions_kill(&harness.core, &caller, kill(&live, false))
        .await
        .unwrap();
    assert!(relayed.body.accepted);
    let command = harness.command(1).await;
    assert_eq!(
        (command.browser_id.as_str(), command.viewer_id.as_str()),
        (BROWSER_FP, BROWSER_FP)
    );
    assert!(
        matches!(&command.frame, ClientControlFrame::Kill { session_id, .. } if session_id.as_str() == live)
    );
}

#[tokio::test]
async fn a_rename_is_trimmed_capped_and_cleared_by_empty() {
    let harness = SessionsHarness::new("rename").await;
    let caller = harness.device(None);
    let sid = session("3");
    harness.seed_session(&sid, 1).await;
    let rename = |title: String| SessionsRenameRequest {
        session_id: sid.clone(),
        title,
        ..Default::default()
    };
    let title_sql = format!("SELECT custom_title FROM sessions WHERE id = '{sid}'");

    let long = format!("  {}  ", "é".repeat(250));
    assert!(
        handle_sessions_rename(&harness.core, &caller, rename(long))
            .await
            .unwrap()
            .body
            .ok
    );
    assert_eq!(harness.text(&title_sql).await, Some("é".repeat(200)));

    assert!(
        handle_sessions_rename(&harness.core, &caller, rename(" \u{feff} ".to_owned()))
            .await
            .unwrap()
            .body
            .ok
    );
    assert_eq!(harness.text(&title_sql).await.unwrap_or_default(), "");

    let missing = SessionsRenameRequest {
        session_id: session("4"),
        title: "x".to_owned(),
        ..Default::default()
    };
    assert!(
        !handle_sessions_rename(&harness.core, &caller, missing)
            .await
            .unwrap()
            .body
            .ok
    );
}

/// The cursor presence deltas published for `sid` while `body` runs.
async fn presence_during<F: Future>(harness: &SessionsHarness, sid: &str, body: F) -> Vec<Value> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let wanted = sid.to_owned();
    let _subscription = harness
        .core
        .services
        .buses
        .global_presence_bus
        .subscribe(move |update| {
            if update.session_id == wanted && update.data["kind"] == "presence-delta" {
                sink.lock().unwrap().push(update.data.clone());
            }
        });
    body.await;
    seen.lock().unwrap().clone()
}

fn route(harness: &SessionsHarness, sid: &str) {
    harness.core.services.byte_hub.replace_worker_channel_index(
        &WorkerFp::try_from(WORKER_FP).unwrap(),
        &[LiveChannel {
            session_id: SessionId::try_from(sid).unwrap(),
            channel_id: ChannelId::try_from(1).unwrap(),
        }],
    );
}

// v2 coord-bidi: "cursor presence uses tab identity while the legacy worker
// envelope uses sender identity" and "... keep the legacy bare fingerprint".
#[tokio::test]
async fn cursor_presence_is_tab_scoped_while_the_worker_envelope_is_the_device() {
    let harness = SessionsHarness::new("cursor").await;
    harness.connect_worker();
    for (tail, tab, viewer) in [
        ("5", Some(TAB), format!("{BROWSER_FP}:{TAB}")),
        ("6", None, BROWSER_FP.to_owned()),
    ] {
        let sid = session(tail);
        harness.seed_session(&sid, 1).await;
        route(&harness, &sid);
        let caller = harness.device(tab);
        let request = SessionsCursorPosRequest {
            session_id: sid.clone(),
            col: 17,
            row: 9,
            ..Default::default()
        };
        let deltas = presence_during(&harness, &sid, async {
            let answer = handle_sessions_cursor_pos(&harness.core, &caller, request)
                .await
                .unwrap();
            assert!(answer.body.accepted);
        })
        .await;
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0]["viewer_id"], json!(viewer));
        assert_eq!(
            (
                deltas[0]["cursor_col"].clone(),
                deltas[0]["cursor_row"].clone()
            ),
            (json!(17), json!(9))
        );
        let sent = harness.commands().last().cloned().unwrap();
        assert_eq!(sent.viewer_id, BROWSER_FP);
        assert!(matches!(
            sent.frame,
            ClientControlFrame::CursorPos {
                col: 17,
                row: 9,
                ..
            }
        ));
    }
    let unknown = SessionsCursorPosRequest {
        session_id: session("7"),
        ..Default::default()
    };
    let answer = handle_sessions_cursor_pos(&harness.core, &harness.device(None), unknown)
        .await
        .unwrap();
    assert!(!answer.body.accepted);
}

#[tokio::test]
async fn attach_relays_the_offset_and_answers_with_the_replay_offset() {
    let harness = SessionsHarness::new("attach").await;
    harness.connect_worker();
    let sid = session("8");
    harness.seed_session(&sid, 1).await;
    let core = harness.core.clone();
    let caller = harness.device(None);
    let request = SessionsAttachRequest {
        session_id: sid.clone(),
        from_offset: Some(40),
        ..Default::default()
    };
    let pending = tokio::spawn(async move {
        handle_sessions_attach(&core, &caller, request)
            .await
            .map(|response| response.body)
    });
    let command = harness.command(1).await;
    assert!(matches!(
        &command.frame,
        ClientControlFrame::Attach {
            from_offset: Some(40),
            ..
        }
    ));
    harness.reply_ok(&command.request_id, json!({ "replay_offset": 42 }));
    assert_eq!(pending.await.unwrap().unwrap().replay_offset, 42);

    let missing = SessionsAttachRequest {
        session_id: session("9"),
        ..Default::default()
    };
    let error = handle_sessions_attach(&harness.core, &harness.device(None), missing)
        .await
        .unwrap_err();
    assert_eq!(
        (error.code, error.message.as_deref()),
        (ErrorCode::NotFound, Some("session not found"))
    );
}

/// Every `sessions-set` delta published while `body` runs, as (workspace, members).
async fn sessions_sets_during<F: Future>(
    harness: &SessionsHarness,
    body: F,
) -> Vec<(String, Vec<String>)> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let _subscription = harness
        .core
        .services
        .buses
        .workspace_bus
        .subscribe(move |delta| {
            if let WorkspaceDelta::SessionsSet {
                id, session_ids, ..
            } = delta
            {
                let members = session_ids
                    .iter()
                    .map(|id| id.as_str().to_owned())
                    .collect();
                sink.lock().unwrap().push((id.as_str().to_owned(), members));
            }
        });
    body.await;
    seen.lock().unwrap().clone()
}

// v2 B5: membership lives in the column AND the junction, and every workspace
// the move touched is republished after commit.
#[tokio::test]
async fn assigning_moves_both_representations_and_republishes_every_touched_workspace() {
    let harness = SessionsHarness::new("assign").await;
    let caller = harness.device(None);
    let sid = session("a");
    let (first, second) = (workspace("1"), workspace("2"));
    harness.seed_session(&sid, 1).await;
    harness.seed_workspace(&first).await;
    harness.seed_workspace(&second).await;
    let assign = |target: Option<&str>| SessionsAssignWorkspaceRequest {
        session_id: sid.clone(),
        workspace_id: target.map(str::to_owned),
        ..Default::default()
    };
    let column = format!("SELECT workspace_id FROM sessions WHERE id = '{sid}'");
    let junction = format!(
        "SELECT string_agg(workspace_id, ',') FROM workspace_sessions WHERE session_id = '{sid}'"
    );

    let sets = sessions_sets_during(&harness, async {
        assert!(
            handle_sessions_assign_workspace(&harness.core, &caller, assign(Some(&first)))
                .await
                .unwrap()
                .body
                .ok
        );
    })
    .await;
    assert_eq!(sets, [(first.clone(), vec![sid.clone()])]);

    let sets = sessions_sets_during(&harness, async {
        assert!(
            handle_sessions_assign_workspace(&harness.core, &caller, assign(Some(&second)))
                .await
                .unwrap()
                .body
                .ok
        );
    })
    .await;
    assert_eq!(
        sets,
        [(second.clone(), vec![sid.clone()]), (first.clone(), vec![])]
    );
    assert_eq!(harness.text(&column).await, Some(second.clone()));
    assert_eq!(harness.text(&junction).await, Some(second.clone()));

    let sets = sessions_sets_during(&harness, async {
        assert!(
            handle_sessions_assign_workspace(&harness.core, &caller, assign(None))
                .await
                .unwrap()
                .body
                .ok
        );
    })
    .await;
    assert_eq!(sets, [(second.clone(), vec![])]);
    assert_eq!(harness.text(&column).await, None);
    assert_eq!(harness.text(&junction).await, None);

    let unknown =
        handle_sessions_assign_workspace(&harness.core, &caller, assign(Some(&workspace("3"))))
            .await;
    assert!(!unknown.unwrap().body.ok);
}
