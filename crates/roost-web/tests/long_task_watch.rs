//! The long-task stall throttle: only a task long enough to freeze the UI is
//! logged, and at most once per burst. Pins `platform::browser::long_task_watch`.

use roost_web::platform::browser::long_task_watch::StallThrottle;

#[test]
fn a_stall_is_reported_once_per_throttle_window() {
    let mut throttle = StallThrottle::default();
    assert!(!throttle.should_log(199.0, 0.0), "below the stall bar");
    assert!(throttle.should_log(250.0, 1_000.0));
    assert!(!throttle.should_log(400.0, 5_000.0), "inside the throttle");
    assert!(throttle.should_log(300.0, 11_000.0));
}
