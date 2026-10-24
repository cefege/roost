//! The surface clamps out-of-range levels, elevations and padding onto their
//! scales, so an out-of-range step still resolves to a real style, and each
//! level stays distinct. Exercises `roost_web::components::ui::surface`.

use roost_web::components::ui::surface::{SurfaceRadius, surface_classes};

#[test]
fn an_out_of_range_level_and_elevation_clamp_onto_the_scale() {
    assert_eq!(
        surface_classes(9, 9, SurfaceRadius::Md, None, false, None),
        surface_classes(3, 5, SurfaceRadius::Md, None, false, None),
    );
}

#[test]
fn each_surface_level_is_distinct() {
    let levels: Vec<String> = (0..=3)
        .map(|level| surface_classes(level, 0, SurfaceRadius::Md, None, false, None))
        .collect();
    for (index, first) in levels.iter().enumerate() {
        for second in &levels[index + 1..] {
            assert_ne!(first, second);
        }
    }
}

#[test]
fn a_padding_below_the_scale_clamps_up_to_its_first_step() {
    assert_eq!(
        surface_classes(0, 0, SurfaceRadius::Xl, Some(0), false, None),
        surface_classes(0, 0, SurfaceRadius::Xl, Some(1), false, None),
    );
}

#[test]
fn a_caller_class_is_appended_after_the_surface_classes() {
    let classes = surface_classes(1, 0, SurfaceRadius::None, None, true, Some("block"));
    assert!(classes.ends_with("block"), "{classes}");
}
