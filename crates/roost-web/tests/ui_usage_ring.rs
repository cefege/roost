//! The usage ring's colour steps up near its limit, its dash offset runs from a
//! full circumference to zero and clamps, and its stroke stays readable at small
//! sizes. Exercises `roost_web::components::ui::usage_ring`.

use roost_web::components::ui::usage_ring::{ring_geometry, usage_ring_offset, usage_ring_token};

#[test]
fn the_token_steps_up_as_the_fill_approaches_the_limit() {
    assert_eq!(usage_ring_token(0.1), "--md-primary");
    assert_eq!(usage_ring_token(0.75), "--status-warn");
    assert_eq!(usage_ring_token(0.91), "--status-warn");
    assert_eq!(usage_ring_token(0.92), "--status-err");
    assert_eq!(usage_ring_token(1.5), "--status-err");
}

#[test]
fn the_offset_runs_from_a_full_circumference_to_zero_and_clamps() {
    let (_, _, circumference) = ring_geometry(18);
    assert!((usage_ring_offset(0.0, circumference) - circumference).abs() < 0.001);
    assert!(usage_ring_offset(1.0, circumference).abs() < 0.001);
    let half = usage_ring_offset(0.5, circumference);
    assert!((half - circumference / 2.0).abs() < 0.001);
    // Over the limit still draws a full ring, not a negative dash offset.
    assert!(usage_ring_offset(1.4, circumference).abs() < 0.001);
    assert!((usage_ring_offset(f64::NAN, circumference) - circumference).abs() < 0.001);
}

#[test]
fn geometry_keeps_a_readable_stroke_at_small_sizes() {
    let (stroke, radius, circumference) = ring_geometry(18);
    assert!(stroke >= 1.5);
    assert!(radius > 0.0);
    assert!(circumference > 0.0);
}
