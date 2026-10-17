//! The progress fill is clamped to its track, so an out-of-range or non-finite
//! fraction never draws outside it. Exercises `roost_web::components::ui::progress`.

use roost_web::components::ui::progress::progress_fill_width;

#[test]
fn the_progress_fill_is_clamped_to_its_track() {
    assert_eq!(progress_fill_width(0.42), "width: 42.0%;");
    assert_eq!(progress_fill_width(1.7), "width: 100.0%;");
    assert_eq!(progress_fill_width(-0.2), "width: 0.0%;");
    assert_eq!(progress_fill_width(f64::NAN), "width: 0.0%;");
}
