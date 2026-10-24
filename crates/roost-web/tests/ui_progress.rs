//! The progress fill is clamped to its track, so an out-of-range or non-finite
//! fraction never draws outside it. Exercises `roost_web::components::ui::progress`.

use roost_web::components::ui::progress::progress_fill_transform;

#[test]
fn the_progress_fill_is_clamped_to_its_track() {
    assert_eq!(progress_fill_transform(0.42), "transform: translateX(-58%)");
    assert_eq!(progress_fill_transform(1.7), "transform: translateX(-0%)");
    assert_eq!(
        progress_fill_transform(-0.2),
        "transform: translateX(-100%)"
    );
    assert_eq!(
        progress_fill_transform(f64::NAN),
        "transform: translateX(-100%)"
    );
}
