//! The legacy UI commands mapped onto a folder's pane arrangement, and the
//! broadcast rule they travel under.
//!
//! Ports `apps/web/tests/uiCommandDispatch.test.ts`: valid commands mirror the
//! user gestures, a bad reference or argument is refused with the input
//! untouched, and the acknowledged apply never enters this mapper. The
//! `reshape_folder_layout` cases pin v2's `applyPure` resolve → map → commit.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod layout_support;

use roost_client_core::client::ui_command::{
    LayoutReshape, LegacyUiCommand, apply_ui_command_to_layout, legacy_frame_accepted,
    reshape_folder_layout,
};
use roost_client_core::store::layout::{
    LayoutRecords, PaneLayout, PaneNode, all_leaves, default_layout, find_leaf, find_leaf_of_tab,
    split_leaf,
};
use roost_protocol::layout::document::LayoutDirection;

use layout_support::{CountedIds, session_ids};

const FOLDER: &str = "worker::/work";

fn live() -> Vec<String> {
    session_ids(&["s1", "s2", "s3"])
}

fn place_split(session_id: &str, anchor: &str, dir: &str, insert_first: bool) -> LegacyUiCommand {
    LegacyUiCommand::PlaceSplit {
        session_id: session_id.to_owned(),
        anchor_session_id: anchor.to_owned(),
        dir: dir.to_owned(),
        insert_first,
    }
}

fn select_tab(session_id: &str) -> LegacyUiCommand {
    LegacyUiCommand::SelectTab {
        session_id: session_id.to_owned(),
    }
}

fn focus_pane(session_id: &str) -> LegacyUiCommand {
    LegacyUiCommand::FocusPane {
        session_id: session_id.to_owned(),
    }
}

fn move_tab(session_id: &str, dest: &str) -> LegacyUiCommand {
    LegacyUiCommand::MoveTab {
        session_id: session_id.to_owned(),
        dest_session_id: dest.to_owned(),
    }
}

fn arrange(preset: &str) -> LegacyUiCommand {
    LegacyUiCommand::Arrange {
        preset: preset.to_owned(),
    }
}

struct Fixture {
    layout: PaneLayout,
    left_pane: String,
    right_pane: String,
    ids: CountedIds,
}

/// Row split: left pane [s1, s2] (s1 selected) | right pane [s3], focus right
/// (a split focuses the pane it creates).
fn fixture() -> Fixture {
    let mut ids = CountedIds::new("pane");
    let layout = default_layout(&live(), &mut ids);
    let root_pane = all_leaves(&layout.root)[0].pane_id.clone();
    let layout = split_leaf(
        &layout,
        &root_pane,
        LayoutDirection::Row,
        "s3",
        false,
        &mut ids,
    );
    let left_pane = find_leaf_of_tab(&layout.root, "s1")
        .unwrap()
        .pane_id
        .clone();
    let right_pane = find_leaf_of_tab(&layout.root, "s3")
        .unwrap()
        .pane_id
        .clone();
    assert_ne!(left_pane, right_pane);
    assert_eq!(layout.focused_pane_id, right_pane);
    Fixture {
        layout,
        left_pane,
        right_pane,
        ids,
    }
}

fn mapped(fixture: &mut Fixture, command: &LegacyUiCommand) -> Option<PaneLayout> {
    apply_ui_command_to_layout(&fixture.layout, command, &live(), &mut fixture.ids)
}

#[test]
fn place_split_creates_a_focused_sibling_pane_holding_the_new_session() {
    let mut fixture = fixture();
    let next = mapped(&mut fixture, &place_split("s4", "s1", "col", false)).unwrap();
    let new_leaf = find_leaf_of_tab(&next.root, "s4").unwrap();
    assert_eq!(new_leaf.tabs, vec!["s4".to_owned()]);
    assert_eq!(new_leaf.selected_tab, "s4");
    assert_eq!(
        next.focused_pane_id, new_leaf.pane_id,
        "focus follows the new pane"
    );
    let anchor = find_leaf(&next.root, &fixture.left_pane).unwrap();
    assert_eq!(anchor.tabs, session_ids(&["s1", "s2"]));
    assert_eq!(all_leaves(&next.root).len(), 3);
}

#[test]
fn place_split_insert_first_puts_the_new_pane_on_the_leading_side() {
    let mut fixture = fixture();
    let next = mapped(&mut fixture, &place_split("s4", "s3", "row", true)).unwrap();
    let order: Vec<&str> = all_leaves(&next.root)
        .iter()
        .map(|leaf| leaf.tabs[0].as_str())
        .collect();
    let placed = order.iter().position(|tab| *tab == "s4").unwrap();
    let anchor = order.iter().position(|tab| *tab == "s3").unwrap();
    assert!(placed < anchor, "{order:?}");
}

#[test]
fn place_split_refuses_an_unknown_anchor_and_an_invalid_direction() {
    let mut fixture = fixture();
    assert_eq!(
        mapped(&mut fixture, &place_split("s4", "nope", "row", false)),
        None
    );
    assert_eq!(
        mapped(&mut fixture, &place_split("s4", "s1", "diagonal", false)),
        None
    );
}

#[test]
fn select_tab_selects_in_its_pane_and_focuses_that_pane() {
    let mut fixture = fixture();
    let next = mapped(&mut fixture, &select_tab("s2")).unwrap();
    assert_eq!(
        find_leaf(&next.root, &fixture.left_pane)
            .unwrap()
            .selected_tab,
        "s2"
    );
    assert_eq!(next.focused_pane_id, fixture.left_pane);
    assert_eq!(mapped(&mut fixture, &select_tab("nope")), None);
}

#[test]
fn focus_pane_focuses_the_pane_containing_the_session_without_selecting_it() {
    let mut fixture = fixture();
    // s2 is a background tab: focus is not select, the deck navigates to s1.
    let next = mapped(&mut fixture, &focus_pane("s2")).unwrap();
    assert_eq!(next.focused_pane_id, fixture.left_pane);
    assert_eq!(
        find_leaf(&next.root, &fixture.left_pane)
            .unwrap()
            .selected_tab,
        "s1"
    );
    assert_eq!(mapped(&mut fixture, &focus_pane("nope")), None);
}

#[test]
fn move_tab_moves_selects_and_collapses_the_emptied_source() {
    let mut fixture = fixture();
    let next = mapped(&mut fixture, &move_tab("s3", "s1")).unwrap();
    let PaneNode::Leaf(only) = &next.root else {
        panic!("the split collapses back to one pane");
    };
    assert_eq!(only.pane_id, fixture.left_pane);
    assert_eq!(only.tabs, session_ids(&["s1", "s2", "s3"]));
    assert_eq!(only.selected_tab, "s3");
    assert_eq!(next.focused_pane_id, fixture.left_pane);
}

#[test]
fn move_tab_refuses_an_unknown_moved_or_destination_session() {
    let mut fixture = fixture();
    assert_eq!(mapped(&mut fixture, &move_tab("nope", "s1")), None);
    assert_eq!(mapped(&mut fixture, &move_tab("s3", "nope")), None);
}

#[test]
fn arrange_balance_keeps_the_tree_and_equalizes_ratios() {
    let mut fixture = fixture();
    let next = mapped(&mut fixture, &arrange("balance")).unwrap();
    let pane_ids = |layout: &PaneLayout| {
        let mut ids: Vec<String> = all_leaves(&layout.root)
            .iter()
            .map(|leaf| leaf.pane_id.clone())
            .collect();
        ids.sort();
        ids
    };
    assert_eq!(pane_ids(&next), pane_ids(&fixture.layout));
    assert_eq!(next.focused_pane_id, fixture.layout.focused_pane_id);
    let PaneNode::Split(split) = &next.root else {
        panic!("balance keeps the split");
    };
    assert_eq!(split.ratio, 0.5, "one leaf each side");
}

#[test]
fn arrange_rebuild_preset_tiles_one_live_session_per_pane() {
    let mut fixture = fixture();
    let next = mapped(&mut fixture, &arrange("even")).unwrap();
    let leaves = all_leaves(&next.root);
    assert_eq!(leaves.len(), live().len());
    let mut tabs: Vec<String> = leaves.iter().flat_map(|leaf| leaf.tabs.clone()).collect();
    tabs.sort();
    assert_eq!(tabs, live());
    assert_eq!(mapped(&mut fixture, &arrange("sideways")), None);
}

#[test]
fn shell_owned_commands_are_not_layout_commands() {
    let mut fixture = fixture();
    let shell_owned = [
        LegacyUiCommand::Navigate {
            path: "/".to_owned(),
        },
        LegacyUiCommand::CloseTab {
            session_id: "s1".to_owned(),
        },
        LegacyUiCommand::Spotlight {
            session_id: "s1".to_owned(),
            off: false,
        },
    ];
    for command in &shell_owned {
        assert_eq!(mapped(&mut fixture, command), None, "{command:?}");
    }
}

#[test]
fn the_input_layout_is_never_mutated() {
    let mut fixture = fixture();
    let before = fixture.layout.clone();
    mapped(&mut fixture, &move_tab("s3", "s1"));
    mapped(&mut fixture, &arrange("even"));
    assert_eq!(fixture.layout, before);
    assert_ne!(fixture.right_pane, fixture.left_pane);
}

#[test]
fn legacy_targeting_accepts_broadcast_and_this_tab_only() {
    assert!(!legacy_frame_accepted("some-other-tab", "own-tab"));
    assert!(legacy_frame_accepted("", "own-tab"));
    assert!(legacy_frame_accepted("own-tab", "own-tab"));
}

#[test]
fn a_committed_place_split_navigates_to_the_placed_session() {
    let mut records = LayoutRecords::new();
    let mut ids = CountedIds::new("pane");
    let outcome = reshape_folder_layout(
        &mut records,
        &mut ids,
        FOLDER,
        &live(),
        &place_split("s3", "s1", "row", false),
    );
    assert_eq!(
        outcome,
        LayoutReshape::Committed {
            navigate_to_session: Some("s3".to_owned())
        }
    );
    let stored = records.stored(FOLDER).unwrap();
    assert_eq!(all_leaves(&stored.root).len(), 2);
    assert_eq!(
        find_leaf_of_tab(&stored.root, "s3").unwrap().pane_id,
        stored.focused_pane_id
    );

    let outcome =
        reshape_folder_layout(&mut records, &mut ids, FOLDER, &live(), &arrange("balance"));
    assert_eq!(
        outcome,
        LayoutReshape::Committed {
            navigate_to_session: None
        }
    );
}

#[test]
fn a_refused_reshape_writes_nothing() {
    let mut records = LayoutRecords::new();
    let mut ids = CountedIds::new("pane");
    let outcome = reshape_folder_layout(
        &mut records,
        &mut ids,
        FOLDER,
        &live(),
        &move_tab("nope", "s1"),
    );
    assert_eq!(outcome, LayoutReshape::Refused);
    assert!(records.is_empty());
}
