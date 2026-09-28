//! Draining the Sync-queued UI commands into shell actions: legacy targeting,
//! session resolution, the store-owned spotlight, and the acknowledged apply
//! kept apart from both.
//!
//! Pins `handleUiCommand` from `apps/web/src/lib/uiCommandDispatch.ts` and the
//! `activeFolder` projection of `apps/web/src/lib/uiLayoutApply.ts`. v2 had no
//! unit test for the shell half; each case here is a v2 branch whose loss a
//! user would see as a command that silently did the wrong thing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod layout_support;
mod ui_command_support;

use roost_client_core::client::ui_command::{
    InboundUiCommand, LegacyUiCommand, UiCommandAction, UiCommandScope, drain_ui_commands,
    layout_apply_folder, read_ui_command_frame,
};
use roost_client_core::store::Store;
use roost_client_core::store::optimistic_spawn::begin_optimistic_spawn;
use roost_client_core::store::paths::ExactWorkerPaths;
use roost_protocol::proto_adapters::layout_document_proto::layout_document_to_proto;

use layout_support::single_pane_document;
use ui_command_support::*;

fn drain(store: &mut Store, active_session_id: Option<&str>) -> Vec<UiCommandAction> {
    let scope = UiCommandScope {
        own_tab_id: OWN_TAB,
        paths: &ExactWorkerPaths,
        active_session_id,
    };
    drain_ui_commands(store, &scope)
}

fn queue(store: &mut Store, frames: Vec<roost_proto::UiCommandFrame>) {
    store.ui_commands.extend(frames);
}

fn open_pending(store: &mut Store) {
    begin_optimistic_spawn(store, PENDING, MACHINE, "/work", None, 150).unwrap();
}

#[test]
fn legacy_commands_honour_broadcast_and_this_tab_and_drop_the_rest() {
    let mut core = seeded_core();
    let store = core.store_mut();
    queue(
        store,
        vec![
            legacy("tab-other", navigate("/other-tab")),
            legacy("", navigate("/broadcast")),
            legacy(OWN_TAB, navigate("/mine")),
            legacy(OWN_TAB, navigate("")),
            frame(OWN_TAB, "", "", None),
        ],
    );
    assert_eq!(
        drain(store, None),
        vec![
            UiCommandAction::Navigate {
                path: "/broadcast".to_owned()
            },
            UiCommandAction::Navigate {
                path: "/mine".to_owned()
            },
        ]
    );
    assert!(store.ui_commands.is_empty(), "every frame is consumed");
}

#[test]
fn an_acknowledged_apply_is_never_filtered_by_legacy_targeting() {
    let document = single_pane_document(&[ALPHA], ALPHA);
    let wire = layout_document_to_proto(&document).unwrap();
    let mut undecodable = wire.clone();
    undecodable.root = roost_proto::buffa::MessageField::none();
    let mut core = seeded_core();
    let store = core.store_mut();
    queue(
        store,
        vec![
            frame(
                "tab-other",
                "socket-1",
                "correlation-1",
                Some(apply_layout(Some(wire))),
            ),
            frame(
                OWN_TAB,
                "socket-1",
                "correlation-2",
                Some(apply_layout(Some(undecodable))),
            ),
        ],
    );
    let actions = drain(store, None);
    let [
        UiCommandAction::ApplyLayout(foreign),
        UiCommandAction::ApplyLayout(broken),
    ] = actions.as_slice()
    else {
        panic!("{actions:?}");
    };
    // Exact-target fencing is the apply core's job, which needs the frame whole.
    assert_eq!(foreign.target_tab_id, "tab-other");
    assert_eq!(foreign.target_socket_id, "socket-1");
    assert_eq!(foreign.correlation_id, "correlation-1");
    assert_eq!(foreign.document.as_ref(), Some(&document));
    // An undecodable document is an invalid-document refusal, not a dropped frame.
    assert_eq!(broken.correlation_id, "correlation-2");
    assert_eq!(broken.document, None);
}

#[test]
fn every_legacy_command_reads_outside_the_acknowledged_path() {
    let commands = [
        navigate("/"),
        place_split(ALPHA, BETA),
        select_tab(ALPHA),
        focus_pane(ALPHA),
        move_tab(ALPHA, BETA),
        arrange("even"),
        close_tab(ALPHA),
        spotlight(ALPHA, false),
    ];
    for command in commands {
        let read =
            read_ui_command_frame(&frame(OWN_TAB, "socket-1", "correlation-1", Some(command)));
        assert!(
            matches!(read, Some(InboundUiCommand::Legacy { .. })),
            "{read:?}"
        );
    }
}

#[test]
fn a_session_reference_must_name_an_open_session() {
    let mut core = seeded_core();
    let store = core.store_mut();
    queue(
        store,
        vec![
            legacy("", select_tab(UNKNOWN)),
            legacy("", select_tab(CLOSED)),
            legacy("", focus_pane(UNKNOWN)),
            legacy("", close_tab(CLOSED)),
            legacy("", select_tab(BETA)),
            legacy("", focus_pane(GAMMA)),
            legacy("", close_tab(ALPHA)),
        ],
    );
    assert_eq!(
        drain(store, None),
        vec![
            UiCommandAction::SelectTab {
                folder_key: work_folder(),
                session_id: BETA.to_owned()
            },
            UiCommandAction::FocusPane {
                folder_key: other_folder(),
                session_id: GAMMA.to_owned()
            },
            UiCommandAction::CloseTab {
                folder_key: work_folder(),
                session_id: ALPHA.to_owned()
            },
        ]
    );
}

#[test]
fn spotlight_is_applied_to_the_store_and_needs_an_open_session() {
    let mut core = seeded_core();
    let store = core.store_mut();
    queue(store, vec![legacy("", spotlight(BETA, false))]);
    assert!(drain(store, None).is_empty());
    assert_eq!(store.spotlight.session_id(), Some(BETA));

    queue(store, vec![legacy("", spotlight(UNKNOWN, false))]);
    drain(store, None);
    assert_eq!(
        store.spotlight.session_id(),
        Some(BETA),
        "an unknown session changes nothing"
    );

    queue(store, vec![legacy("", spotlight("", true))]);
    drain(store, None);
    assert_eq!(store.spotlight.session_id(), None);
}

#[test]
fn reshapes_need_both_ends_open_and_target_the_named_folder() {
    let mut core = seeded_core();
    let store = core.store_mut();
    queue(
        store,
        vec![
            legacy("", place_split(UNKNOWN, ALPHA)),
            legacy("", place_split(BETA, CLOSED)),
            legacy("", move_tab(ALPHA, UNKNOWN)),
            legacy("", place_split(GAMMA, ALPHA)),
            legacy("", move_tab(ALPHA, GAMMA)),
        ],
    );
    let actions = drain(store, None);
    assert_eq!(actions.len(), 2, "{actions:?}");
    let UiCommandAction::ReshapeLayout {
        folder_key,
        live_session_ids,
        command,
    } = &actions[0]
    else {
        panic!("{actions:?}");
    };
    assert_eq!(
        folder_key,
        &work_folder(),
        "place_split lands in the anchor's folder"
    );
    assert_eq!(live_session_ids, &vec![ALPHA.to_owned(), BETA.to_owned()]);
    assert!(matches!(command, LegacyUiCommand::PlaceSplit { .. }));
    let UiCommandAction::ReshapeLayout { folder_key, .. } = &actions[1] else {
        panic!("{actions:?}");
    };
    assert_eq!(
        folder_key,
        &other_folder(),
        "move_tab lands in the destination's folder"
    );
}

#[test]
fn arrange_targets_the_folder_the_route_is_viewing() {
    let mut core = seeded_core();
    let store = core.store_mut();
    queue(store, vec![legacy("", arrange("even"))]);
    assert!(
        drain(store, None).is_empty(),
        "no viewed folder, nothing to arrange"
    );

    queue(store, vec![legacy("", arrange("even"))]);
    assert!(
        drain(store, Some(CLOSED)).is_empty(),
        "a closed route session is no folder"
    );

    queue(store, vec![legacy("", arrange("even"))]);
    let actions = drain(store, Some(GAMMA));
    assert!(
        matches!(
            actions.as_slice(),
            [UiCommandAction::ReshapeLayout { folder_key, .. }] if *folder_key == other_folder()
        ),
        "{actions:?}"
    );
}

#[test]
fn a_pending_spawn_is_a_session_commands_may_name_and_a_folder_member() {
    let mut core = seeded_core();
    let store = core.store_mut();
    open_pending(store);
    queue(
        store,
        vec![legacy("", close_tab(PENDING)), legacy("", arrange("even"))],
    );
    let actions = drain(store, Some(ALPHA));
    assert_eq!(
        actions[0],
        UiCommandAction::CloseTab {
            folder_key: work_folder(),
            session_id: PENDING.to_owned()
        }
    );
    let UiCommandAction::ReshapeLayout {
        live_session_ids, ..
    } = &actions[1]
    else {
        panic!("{actions:?}");
    };
    assert_eq!(
        live_session_ids,
        &vec![ALPHA.to_owned(), PENDING.to_owned(), BETA.to_owned()]
    );
}

#[test]
fn the_apply_folder_projects_out_client_only_membership() {
    let mut core = seeded_core();
    let store = core.store_mut();
    let folder = layout_apply_folder(store, &ExactWorkerPaths, Some(ALPHA)).unwrap();
    assert_eq!(folder.folder_key, work_folder());
    assert_eq!(folder.active_session_id, ALPHA);
    assert_eq!(
        folder.live_session_ids,
        vec![ALPHA.to_owned(), BETA.to_owned()]
    );
    assert!(!folder.has_client_only_session);
    assert_eq!(layout_apply_folder(store, &ExactWorkerPaths, None), None);
    assert_eq!(
        layout_apply_folder(store, &ExactWorkerPaths, Some(CLOSED)),
        None
    );

    open_pending(store);
    let sibling = layout_apply_folder(store, &ExactWorkerPaths, Some(ALPHA)).unwrap();
    assert_eq!(
        sibling.live_session_ids,
        vec![ALPHA.to_owned(), BETA.to_owned()]
    );
    assert!(sibling.has_client_only_session);
    let active = layout_apply_folder(store, &ExactWorkerPaths, Some(PENDING)).unwrap();
    assert_eq!(active.active_session_id, PENDING);
    assert!(active.has_client_only_session);
    assert!(!active.live_session_ids.contains(&PENDING.to_owned()));
}
