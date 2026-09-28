//! The key pad's first-key focus retry: a pad with no pointer must land focus
//! on the first key once the sheet has mounted, stop the moment focus took,
//! give up after a bounded number of frames, and stand down when cancelled.
//! Ports the focus half of `apps/web/tests/terminalNavButtons.padSeam.dom.test.ts`.

use roost_web::input_nav::keypad_focus::{
    FIRST_KEY_FOCUS_ATTEMPTS, FirstKeyFocusRetry, KEYPAD_FIRST_KEY_SELECTOR, RetryStep,
};

#[test]
fn focuses_the_first_grid_key_once_it_mounts() {
    assert!(KEYPAD_FIRST_KEY_SELECTOR.contains(".term-nav__grid"));
    let mut retry = FirstKeyFocusRetry::new();

    // The microtask attempt finds no sheet yet: one more frame is requested.
    assert!(retry.may_attempt());
    assert_eq!(retry.record_attempt(false), RetryStep::NextFrame);

    // The key mounted and took focus: the retry must stop instead of
    // re-focusing every frame.
    assert!(retry.may_attempt());
    assert_eq!(retry.record_attempt(true), RetryStep::Stop);
}

#[test]
fn the_retry_is_bounded() {
    let mut retry = FirstKeyFocusRetry::new();
    let mut attempts = 0;
    while retry.may_attempt() {
        attempts += 1;
        if retry.record_attempt(false) == RetryStep::Stop {
            break;
        }
    }
    assert_eq!(attempts, FIRST_KEY_FOCUS_ATTEMPTS);
    assert!(!retry.may_attempt());
}

#[test]
fn a_cancelled_retry_never_focuses() {
    let mut retry = FirstKeyFocusRetry::new();
    assert_eq!(retry.record_attempt(false), RetryStep::NextFrame);

    retry.cancel();

    assert!(!retry.may_attempt(), "a pending frame after cancel must not focus");
    assert_eq!(retry.record_attempt(false), RetryStep::Stop);
}
