//! The select's behaviour as data: what choosing reports, what each key does on
//! the trigger and in the listbox, how type-ahead finds an option, and where the
//! listbox is placed. Exercises `roost_web::components::md::{select,
//! select_navigation, select_placement}`, the port of
//! `apps/web/src/components/Settings/md/Select.tsx` and the Kobalte select
//! behaviour it relied on.

use roost_web::components::md::select::{SelectOption, select_change, select_disabled};
use roost_web::components::md::select_navigation::{
    ListboxKeyAction, OpenFocus, TYPEAHEAD_RESET_MS, TriggerKeyAction, Typeahead,
    initial_highlight, listbox_key_action, trigger_key_action, typeahead_character,
    typeahead_match,
};
use roost_web::components::md::select_placement::{
    AnchorRect, PlacementSide, SELECT_GUTTER_PX, listbox_placement, listbox_style,
};

fn options() -> Vec<SelectOption> {
    vec![
        SelectOption::new("surface", "Surface"),
        SelectOption::new("accent", "Accent"),
        SelectOption::new("status", "Status"),
    ]
}

#[test]
fn choosing_the_current_option_reports_nothing() {
    // v2's guard: re-choosing the value already shown is not a save, so a
    // rejected-save spec sees exactly one request.
    assert_eq!(select_change(&options(), "accent", 1), None);
    assert_eq!(
        select_change(&options(), "accent", 2).as_deref(),
        Some("status")
    );
    assert_eq!(select_change(&options(), "accent", 9), None);
}

#[test]
fn an_empty_select_is_disabled() {
    assert!(select_disabled(false, 0));
    assert!(select_disabled(true, 3));
    assert!(!select_disabled(false, 3));
}

#[test]
fn opening_highlights_the_selection_before_either_end() {
    assert_eq!(initial_highlight(3, Some(1), OpenFocus::First), Some(1));
    assert_eq!(initial_highlight(3, Some(1), OpenFocus::Last), Some(1));
    assert_eq!(initial_highlight(3, None, OpenFocus::First), Some(0));
    assert_eq!(initial_highlight(3, None, OpenFocus::Last), Some(2));
    assert_eq!(initial_highlight(0, None, OpenFocus::First), None);
}

#[test]
fn the_trigger_opens_on_enter_space_and_the_vertical_arrows() {
    for key in ["Enter", " ", "ArrowDown"] {
        assert_eq!(
            trigger_key_action(key, 3, None),
            TriggerKeyAction::Open(OpenFocus::First),
            "{key:?}"
        );
    }
    assert_eq!(
        trigger_key_action("ArrowUp", 3, Some(2)),
        TriggerKeyAction::Open(OpenFocus::Last)
    );
    assert_eq!(
        trigger_key_action("Escape", 3, None),
        TriggerKeyAction::Ignore
    );
}

#[test]
fn the_horizontal_arrows_step_the_value_without_wrapping() {
    assert_eq!(
        trigger_key_action("ArrowRight", 3, Some(0)),
        TriggerKeyAction::Choose(1)
    );
    assert_eq!(
        trigger_key_action("ArrowRight", 3, Some(2)),
        TriggerKeyAction::Ignore
    );
    assert_eq!(
        trigger_key_action("ArrowLeft", 3, Some(2)),
        TriggerKeyAction::Choose(1)
    );
    assert_eq!(
        trigger_key_action("ArrowLeft", 3, Some(0)),
        TriggerKeyAction::Ignore
    );
    assert_eq!(
        trigger_key_action("ArrowLeft", 3, None),
        TriggerKeyAction::Choose(0)
    );
    assert_eq!(
        trigger_key_action("ArrowRight", 0, None),
        TriggerKeyAction::Ignore
    );
}

#[test]
fn listbox_movement_stops_at_the_ends() {
    assert_eq!(
        listbox_key_action("ArrowDown", 3, Some(0)),
        ListboxKeyAction::Highlight(1)
    );
    assert_eq!(
        listbox_key_action("ArrowDown", 3, Some(2)),
        ListboxKeyAction::Highlight(2)
    );
    assert_eq!(
        listbox_key_action("ArrowUp", 3, Some(0)),
        ListboxKeyAction::Highlight(0)
    );
    assert_eq!(
        listbox_key_action("ArrowUp", 3, None),
        ListboxKeyAction::Highlight(2)
    );
    assert_eq!(
        listbox_key_action("Home", 3, Some(2)),
        ListboxKeyAction::Highlight(0)
    );
    assert_eq!(
        listbox_key_action("End", 3, Some(0)),
        ListboxKeyAction::Highlight(2)
    );
}

#[test]
fn the_listbox_chooses_the_highlight_and_closes_on_escape_and_tab() {
    assert_eq!(
        listbox_key_action("Enter", 3, Some(1)),
        ListboxKeyAction::Choose(1)
    );
    assert_eq!(
        listbox_key_action(" ", 3, Some(2)),
        ListboxKeyAction::Choose(2)
    );
    assert_eq!(
        listbox_key_action("Enter", 3, None),
        ListboxKeyAction::Ignore
    );
    assert_eq!(
        listbox_key_action("Escape", 3, Some(1)),
        ListboxKeyAction::Close
    );
    assert_eq!(listbox_key_action("Tab", 0, None), ListboxKeyAction::Close);
    assert_eq!(
        listbox_key_action("ArrowDown", 0, None),
        ListboxKeyAction::Ignore
    );
}

#[test]
fn typeahead_takes_single_characters_but_not_chords_or_named_keys() {
    assert_eq!(typeahead_character("a", false), Some("a"));
    assert_eq!(typeahead_character("a", true), None);
    assert_eq!(typeahead_character("ArrowDown", false), None);
}

#[test]
fn typeahead_accumulates_a_prefix_and_restarts_after_a_quiet_second() {
    let mut typeahead = Typeahead::default();
    assert!(!typeahead.is_active(0.0));
    assert_eq!(typeahead.push("s", 0.0), "s");
    assert_eq!(typeahead.push("t", 400.0), "st");
    assert!(typeahead.is_active(400.0 + TYPEAHEAD_RESET_MS));
    assert!(!typeahead.is_active(400.0 + TYPEAHEAD_RESET_MS + 1.0));
    assert_eq!(typeahead.push("a", 400.0 + TYPEAHEAD_RESET_MS + 1.0), "a");
}

#[test]
fn typeahead_matches_from_the_highlight_then_from_the_top() {
    let labels = ["Surface", "Accent", "Status"];
    assert_eq!(typeahead_match(&labels, "s", Some(1)), Some(2));
    assert_eq!(typeahead_match(&labels, "s", Some(0)), Some(0));
    assert_eq!(typeahead_match(&labels, "su", Some(2)), Some(0));
    assert_eq!(typeahead_match(&labels, "ACC", None), Some(1));
    assert_eq!(typeahead_match(&labels, "x", None), None);
}

#[test]
fn the_listbox_opens_below_its_trigger_at_the_trigger_width() {
    let anchor = AnchorRect {
        left: 40.0,
        top: 100.0,
        width: 220.0,
        height: 36.0,
    };
    let placement = listbox_placement(anchor, Some(120.0), Some(800.0));
    assert_eq!(placement.side, PlacementSide::Bottom);
    assert_eq!(placement.top, 100.0 + 36.0 + SELECT_GUTTER_PX);
    assert_eq!((placement.left, placement.width), (40.0, 220.0));
    assert_eq!(listbox_placement(anchor, None, None), placement);
}

#[test]
fn the_listbox_flips_above_only_when_below_overflows_and_above_has_more_room() {
    let near_bottom = AnchorRect {
        left: 0.0,
        top: 700.0,
        width: 200.0,
        height: 36.0,
    };
    let flipped = listbox_placement(near_bottom, Some(200.0), Some(800.0));
    assert_eq!(flipped.side, PlacementSide::Top);
    assert_eq!(flipped.top, 700.0 - SELECT_GUTTER_PX - 200.0);
    assert!(listbox_style(&flipped).contains("--kb-select-content-transform-origin: bottom;"));

    let near_top = AnchorRect {
        left: 0.0,
        top: 20.0,
        width: 200.0,
        height: 36.0,
    };
    let cramped = listbox_placement(near_top, Some(900.0), Some(800.0));
    assert_eq!(cramped.side, PlacementSide::Bottom);
}
