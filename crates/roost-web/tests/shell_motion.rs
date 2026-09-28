//! The shell's pointer and motion rules: the drag-arming gate, the drawer's
//! swipe geometry, drag-to-tile zones and the spring solver. Ports
//! `apps/web/tests/{dragThreshold,edgeSwipeDrawer,dropZones,spring}.test.ts`
//! against `roost_web::motion`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_web::motion::drag_threshold::{DRAG_THRESHOLD_PX, drag_armed};
use roost_web::motion::drop_zones::{
    DropZone, PaneBox, Rect, SplitDir, SplitPlacement, drop_zone_for, tile_target_for, zone_rect,
    zone_to_split,
};
use roost_web::motion::edge_swipe_drawer::{
    LockedAxis, close_offset_px, lock_axis, open_offset_px, should_close, should_open,
};
use roost_web::motion::spring::{
    SPRING_REST_POSITION, SPRING_REST_VELOCITY, SPRING_SNAP, SpringState, critical_damping,
    is_spring_at_rest, spring_step,
};

#[test]
fn a_straight_down_split_drag_arms() {
    assert!(drag_armed(100.0, 100.0, 100.0, 100.0 + DRAG_THRESHOLD_PX + 3.0));
}

#[test]
fn every_direction_past_the_threshold_arms() {
    assert!(drag_armed(100.0, 100.0, 100.0, 91.0));
    assert!(drag_armed(100.0, 100.0, 109.0, 100.0));
    assert!(drag_armed(100.0, 100.0, 91.0, 100.0));
    assert!(drag_armed(100.0, 100.0, 106.0, 108.0));
}

#[test]
fn travel_under_the_threshold_never_arms() {
    assert!(!drag_armed(100.0, 100.0, 100.0, 104.0));
    assert!(!drag_armed(100.0, 100.0, 102.0, 102.0));
    assert!(!drag_armed(100.0, 100.0, 100.0, 100.0));
}

#[test]
fn the_axis_locks_only_past_the_arm_gate_and_horizontal_needs_the_ratio() {
    assert_eq!(lock_axis(2.0, 3.0), LockedAxis::None);
    assert_eq!(lock_axis(20.0, 3.0), LockedAxis::X);
    assert_eq!(lock_axis(3.0, 20.0), LockedAxis::Y);
    assert_eq!(lock_axis(20.0, 20.0), LockedAxis::Y);
}

#[test]
fn the_open_offset_follows_the_finger_within_the_drawer_width() {
    assert_eq!(open_offset_px(0.0, 400.0), -400.0);
    assert_eq!(open_offset_px(400.0, 400.0), 0.0);
    assert_eq!(open_offset_px(600.0, 400.0), 0.0);
    assert_eq!(open_offset_px(-50.0, 400.0), -400.0);
}

#[test]
fn an_open_commits_past_thirty_percent_or_on_a_rightward_flick_only() {
    assert!(should_open(120.0, 0.0, 400.0));
    assert!(!should_open(119.0, 0.0, 400.0));
    assert!(should_open(10.0, 0.8, 400.0));
    assert!(!should_open(10.0, 0.79, 400.0));
    assert!(!should_open(-200.0, 0.0, 400.0));
}

#[test]
fn the_close_offset_follows_the_finger_leftward_only() {
    assert_eq!(close_offset_px(0.0, 400.0), 0.0);
    assert_eq!(close_offset_px(-400.0, 400.0), -400.0);
    assert_eq!(close_offset_px(-600.0, 400.0), -400.0);
    assert_eq!(close_offset_px(50.0, 400.0), 0.0);
}

#[test]
fn a_close_commits_past_thirty_percent_left_or_on_a_leftward_flick_only() {
    assert!(should_close(-120.0, 0.0, 400.0));
    assert!(!should_close(-119.0, 0.0, 400.0));
    assert!(should_close(-10.0, -0.8, 400.0));
    assert!(!should_close(-10.0, -0.79, 400.0));
    assert!(!should_close(200.0, 0.0, 400.0));
}

const PANE: Rect = Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 };

#[test]
fn the_centre_merges_and_each_outer_band_splits_on_its_edge() {
    assert_eq!(drop_zone_for(PANE, 400.0, 300.0), DropZone::Center);
    assert_eq!(drop_zone_for(PANE, 20.0, 300.0), DropZone::Left);
    assert_eq!(drop_zone_for(PANE, 790.0, 300.0), DropZone::Right);
    assert_eq!(drop_zone_for(PANE, 400.0, 10.0), DropZone::Top);
    assert_eq!(drop_zone_for(PANE, 400.0, 590.0), DropZone::Bottom);
}

#[test]
fn a_small_pane_still_has_an_eighty_pixel_band() {
    let small = Rect { x: 0.0, y: 0.0, w: 200.0, h: 200.0 };
    assert_eq!(drop_zone_for(small, 70.0, 100.0), DropZone::Left);
    assert_eq!(drop_zone_for(small, 100.0, 100.0), DropZone::Center);
}

#[test]
fn a_corner_tie_favours_the_horizontal_edge_and_the_origin_is_respected() {
    assert_eq!(drop_zone_for(PANE, 10.0, 10.0), DropZone::Left);
    let offset = Rect { x: 400.0, y: 300.0, w: 800.0, h: 600.0 };
    assert_eq!(drop_zone_for(offset, 800.0, 600.0), DropZone::Center);
    assert_eq!(drop_zone_for(offset, 420.0, 600.0), DropZone::Left);
}

#[test]
fn edges_map_to_splits_and_merge_or_reorder_do_not() {
    let split = |dir, insert_first| Some(SplitPlacement { dir, insert_first });
    assert_eq!(zone_to_split(DropZone::Left), split(SplitDir::Row, true));
    assert_eq!(zone_to_split(DropZone::Right), split(SplitDir::Row, false));
    assert_eq!(zone_to_split(DropZone::Top), split(SplitDir::Col, true));
    assert_eq!(zone_to_split(DropZone::Bottom), split(SplitDir::Col, false));
    assert_eq!(zone_to_split(DropZone::Center), None);
    assert_eq!(zone_to_split(DropZone::Reorder), None);
}

#[test]
fn a_zone_highlights_the_half_the_new_pane_takes() {
    let rect = |x, y, w, h| Rect { x, y, w, h };
    assert_eq!(zone_rect(PANE, DropZone::Left), rect(0.0, 0.0, 400.0, 600.0));
    assert_eq!(zone_rect(PANE, DropZone::Right), rect(400.0, 0.0, 400.0, 600.0));
    assert_eq!(zone_rect(PANE, DropZone::Top), rect(0.0, 0.0, 800.0, 300.0));
    assert_eq!(zone_rect(PANE, DropZone::Bottom), rect(0.0, 300.0, 800.0, 300.0));
    assert_eq!(zone_rect(PANE, DropZone::Center), PANE);
    assert_eq!(zone_rect(PANE, DropZone::Reorder), PANE);
}

fn panes() -> Vec<PaneBox> {
    vec![
        PaneBox { pane_id: "home".into(), rect: Rect { x: 0.0, y: 0.0, w: 400.0, h: 600.0 } },
        PaneBox { pane_id: "other".into(), rect: Rect { x: 400.0, y: 0.0, w: 400.0, h: 600.0 } },
    ]
}

fn zone_at(x: f64, y: f64) -> Option<DropZone> {
    tile_target_for(&panes(), "home", x, y, 40.0).map(|target| target.zone)
}

#[test]
fn the_home_pane_body_reorders_its_edges_split_and_its_strip_is_left_to_the_strip() {
    let target = tile_target_for(&panes(), "home", 200.0, 300.0, 40.0).unwrap();
    assert_eq!((target.pane_id.as_str(), target.zone), ("home", DropZone::Reorder));
    assert_eq!(zone_at(10.0, 300.0), Some(DropZone::Left));
    assert_eq!(zone_at(390.0, 300.0), Some(DropZone::Right));
    assert_eq!(zone_at(200.0, 590.0), Some(DropZone::Bottom));
    assert_eq!(zone_at(200.0, 10.0), None);
}

#[test]
fn another_pane_merges_on_its_centre_or_strip_and_splits_on_its_edges() {
    assert_eq!(zone_at(600.0, 300.0), Some(DropZone::Center));
    assert_eq!(zone_at(600.0, 10.0), Some(DropZone::Center));
    assert_eq!(zone_at(410.0, 300.0), Some(DropZone::Left));
    assert_eq!(zone_at(790.0, 300.0), Some(DropZone::Right));
}

#[test]
fn a_pointer_off_every_pane_targets_nothing() {
    assert_eq!(zone_at(900.0, 300.0), None);
    assert_eq!(zone_at(200.0, 900.0), None);
}

#[test]
fn critical_damping_is_two_root_k_m() {
    assert!((critical_damping(100.0, 1.0) - 20.0).abs() < 1e-9);
    assert!((critical_damping(100.0, 4.0) - 40.0).abs() < 1e-9);
    assert!((critical_damping(400.0, 1.0) - 40.0).abs() < 1e-9);
}

#[test]
fn a_zero_or_negative_step_changes_nothing() {
    let state = SpringState { position: 10.0, velocity: 5.0 };
    assert_eq!(spring_step(state, 0.0, SPRING_SNAP, 0.0), state);
    assert_eq!(spring_step(state, 0.0, SPRING_SNAP, -16.0), state);
}

#[test]
fn a_step_pulls_toward_the_target_and_a_resting_spring_stays() {
    let next = spring_step(SpringState { position: 100.0, velocity: 0.0 }, 0.0, SPRING_SNAP, 16.0);
    assert!(next.position < 100.0 && next.velocity < 0.0);
    let still = spring_step(SpringState { position: 0.0, velocity: 0.0 }, 0.0, SPRING_SNAP, 16.0);
    assert!(still.position.abs() < 1e-9 && still.velocity.abs() < 1e-9);
}

#[test]
fn rest_needs_both_close_and_slow() {
    assert!(is_spring_at_rest(SpringState { position: 0.05, velocity: 0.5 }, 0.0));
    assert!(!is_spring_at_rest(SpringState { position: 5.0, velocity: 0.0 }, 0.0));
    assert!(!is_spring_at_rest(SpringState { position: 0.0, velocity: 100.0 }, 0.0));
    let inside = SpringState { position: SPRING_REST_POSITION / 2.0, velocity: SPRING_REST_VELOCITY / 2.0 };
    assert!(is_spring_at_rest(inside, 0.0));
}

#[test]
fn the_snap_spring_settles_within_two_seconds_of_frames() {
    let mut state = SpringState { position: 200.0, velocity: 0.0 };
    let mut frames = 0;
    while !is_spring_at_rest(state, 0.0) && frames < 120 {
        state = spring_step(state, 0.0, SPRING_SNAP, 16.0);
        frames += 1;
    }
    assert!(is_spring_at_rest(state, 0.0));
    assert!(frames < 120);
}
