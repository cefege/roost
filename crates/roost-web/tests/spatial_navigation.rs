//! Pins the geometry rule a TV remote depends on: an arrow press moves focus to
//! the control the user is pointing at, not merely the nearest one; the first
//! press from `<body>` lands somewhere; and the arrow is left alone wherever
//! something else owns it.
//! Ports `apps/web/tests/spatialNavigation.test.ts`, plus the guard for
//! `docs/FAILURE-INDEX.md` "The first D-pad press does nothing because `<body>`
//! counts as the origin".

use roost_web::input_nav::spatial::{
    ArrowKeydown, Direction, FocusedArrowOwner, NavRect, best_candidate_in_direction,
    claimable_direction, pick_target,
};
use roost_web_terminal::reader_intent::ScrollBoxGeometry;

/// Mid-screen: left 100, top 100, right 200, bottom 150.
fn origin() -> NavRect {
    NavRect::new(100.0, 100.0, 100.0, 50.0)
}

#[test]
fn each_arrow_picks_the_control_on_that_side_of_the_origin() {
    let above = NavRect::new(100.0, 0.0, 100.0, 50.0);
    let below = NavRect::new(100.0, 300.0, 100.0, 50.0);
    let to_left = NavRect::new(0.0, 100.0, 50.0, 50.0);
    let to_right = NavRect::new(400.0, 100.0, 100.0, 50.0);
    let all = [above, below, to_left, to_right];

    assert_eq!(best_candidate_in_direction(&origin(), &all, Direction::Up), Some(0));
    assert_eq!(best_candidate_in_direction(&origin(), &all, Direction::Down), Some(1));
    assert_eq!(best_candidate_in_direction(&origin(), &all, Direction::Left), Some(2));
    assert_eq!(best_candidate_in_direction(&origin(), &all, Direction::Right), Some(3));
}

#[test]
fn a_nearer_but_far_off_axis_candidate_loses_to_a_farther_on_axis_one() {
    let nearer_but_sideways = NavRect::new(900.0, 200.0, 100.0, 50.0);
    let straight_down = NavRect::new(100.0, 300.0, 100.0, 50.0);

    assert_eq!(
        best_candidate_in_direction(&origin(), &[nearer_but_sideways, straight_down], Direction::Down),
        Some(1)
    );
}

#[test]
fn returns_none_when_nothing_lies_in_the_requested_direction() {
    let above = NavRect::new(100.0, 0.0, 100.0, 50.0);

    assert_eq!(best_candidate_in_direction(&origin(), &[above], Direction::Down), None);
    assert_eq!(best_candidate_in_direction(&origin(), &[], Direction::Up), None);
}

#[test]
fn a_candidate_overlapping_the_origin_edge_is_not_a_move() {
    // Centre inside the origin's own band: travelling there would not advance
    // focus, so the scorer must reject it rather than re-focus in place.
    let overlapping = NavRect::new(100.0, 120.0, 100.0, 20.0);

    assert_eq!(best_candidate_in_direction(&origin(), &[overlapping], Direction::Down), None);
    assert_eq!(best_candidate_in_direction(&origin(), &[overlapping], Direction::Up), None);
}

#[test]
fn with_no_origin_the_first_press_lands_on_the_topmost_leftmost_control() {
    // Focus on <body> after load: the page's own box contains every control, so
    // using it as the origin would find nothing beyond it in any direction.
    let lower = NavRect::new(0.0, 400.0, 100.0, 40.0);
    let top_right = NavRect::new(600.0, 20.0, 100.0, 40.0);
    let top_left = NavRect::new(40.0, 20.0, 100.0, 40.0);
    let candidates = [lower, top_right, top_left];

    assert_eq!(pick_target(None, &candidates, Direction::Down), Some(2));
    assert_eq!(pick_target(None, &candidates, Direction::Up), Some(2));
    // A zero-size origin (a hidden element kept focus) is no origin either.
    let collapsed = NavRect::new(300.0, 300.0, 0.0, 0.0);
    assert_eq!(pick_target(Some(&collapsed), &candidates, Direction::Down), Some(2));
}

#[test]
fn a_real_origin_travels_by_geometry() {
    let below = NavRect::new(100.0, 300.0, 100.0, 50.0);
    let top_left = NavRect::new(0.0, 0.0, 10.0, 10.0);
    assert_eq!(pick_target(Some(&origin()), &[top_left, below], Direction::Down), Some(1));
}

fn arrow(key: &str) -> ArrowKeydown<'_> {
    ArrowKeydown { key, default_prevented: false, modified: false }
}

#[test]
fn the_gate_claims_only_unclaimed_bare_arrows_in_a_directional_modality() {
    assert_eq!(claimable_direction(true, &arrow("ArrowDown"), None), Some(Direction::Down));
    assert_eq!(claimable_direction(false, &arrow("ArrowDown"), None), None);
    assert_eq!(claimable_direction(true, &arrow("Enter"), None), None);
    let prevented = ArrowKeydown { default_prevented: true, ..arrow("ArrowDown") };
    assert_eq!(claimable_direction(true, &prevented, None), None);
    let chord = ArrowKeydown { modified: true, ..arrow("ArrowLeft") };
    assert_eq!(claimable_direction(true, &chord, None), None);
}

#[test]
fn the_gate_leaves_arrows_to_carets_and_roving_surfaces() {
    let caret = FocusedArrowOwner { editable: true, ..FocusedArrowOwner::default() };
    let menu = FocusedArrowOwner { in_roving_role: true, ..FocusedArrowOwner::default() };
    let button = FocusedArrowOwner::default();

    assert_eq!(claimable_direction(true, &arrow("ArrowUp"), Some(&caret)), None);
    assert_eq!(claimable_direction(true, &arrow("ArrowUp"), Some(&menu)), None);
    assert_eq!(claimable_direction(true, &arrow("ArrowUp"), Some(&button)), Some(Direction::Up));
}

#[test]
fn a_focused_scroll_box_keeps_vertical_arrows_until_it_clamps() {
    let terminal = |scroll_top: f64| FocusedArrowOwner {
        terminal_scroll_box: Some(ScrollBoxGeometry { scroll_top, scroll_height: 1000.0, client_height: 400.0 }),
        ..FocusedArrowOwner::default()
    };
    let middle = terminal(300.0);
    assert_eq!(claimable_direction(true, &arrow("ArrowUp"), Some(&middle)), None);
    assert_eq!(claimable_direction(true, &arrow("ArrowDown"), Some(&middle)), None);
    // Sideways never belongs to the box.
    assert_eq!(claimable_direction(true, &arrow("ArrowRight"), Some(&middle)), Some(Direction::Right));
    // At an edge focus leaves the pane instead of dead-ending.
    assert_eq!(claimable_direction(true, &arrow("ArrowUp"), Some(&terminal(0.0))), Some(Direction::Up));
    assert_eq!(claimable_direction(true, &arrow("ArrowDown"), Some(&terminal(600.0))), Some(Direction::Down));
}
