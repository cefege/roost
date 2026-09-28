//! The WebSocket authentication deadline: the socket closes `4003 reauth
//! required` exactly at the credential's deadline, and a deadline beyond the
//! timer maximum is re-armed rather than dropped.
//!
//! Ports `apps/coord/tests/auth/ws-auth-deadline.test.ts` over a paused tokio
//! clock, so no test waits the deadline out.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use roost_coord::auth::ws_auth_deadline::{reauth_expired, reauth_wait_ms, sleep_until_reauth_on};

/// The v2 fake clock's `maxDelayMs`.
const FAKE_MAX_DELAY_MS: u64 = 50;

/// Arm the deadline on a task whose clock is the paused runtime's elapsed time,
/// and report when it fired.
fn arm_deadline(deadline_ms: i64) -> Arc<AtomicBool> {
    let fired = Arc::new(AtomicBool::new(false));
    let start = tokio::time::Instant::now();
    let flag = Arc::clone(&fired);
    tokio::spawn(async move {
        let clock = move || i64::try_from(start.elapsed().as_millis()).unwrap();
        sleep_until_reauth_on(deadline_ms, FAKE_MAX_DELAY_MS, &clock).await;
        flag.store(true, Ordering::SeqCst);
    });
    fired
}

async fn advance(ms: u64) {
    tokio::time::advance(Duration::from_millis(ms)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

// v2: "closes at Access expiry with the reauth code and reason"
#[tokio::test(start_paused = true)]
async fn fires_exactly_at_the_deadline_and_not_before() {
    let fired = arm_deadline(100);
    advance(0).await;
    advance(99).await;
    assert!(!fired.load(Ordering::SeqCst), "one millisecond early");
    advance(1).await;
    assert!(fired.load(Ordering::SeqCst), "at the deadline");
}

// v2: "re-arms deadlines beyond the platform timer maximum"
#[tokio::test(start_paused = true)]
async fn re_arms_a_deadline_beyond_the_timer_maximum() {
    let fired = arm_deadline(120);
    advance(0).await;
    advance(50).await;
    assert!(!fired.load(Ordering::SeqCst));
    advance(50).await;
    assert!(!fired.load(Ordering::SeqCst));
    advance(20).await;
    assert!(fired.load(Ordering::SeqCst));
}

// v2 sync-ws-handler.ts:187-191: a deadline already passed at open closes
// immediately, and the instant itself counts as passed.
#[test]
fn a_deadline_at_or_before_now_is_already_expired() {
    assert!(reauth_expired(100, 100));
    assert!(reauth_expired(99, 100));
    assert!(!reauth_expired(101, 100));
    assert_eq!(reauth_wait_ms(100, 100, FAKE_MAX_DELAY_MS), None);
    assert_eq!(reauth_wait_ms(101, 100, FAKE_MAX_DELAY_MS), Some(1));
    assert_eq!(reauth_wait_ms(i64::MAX, 0, FAKE_MAX_DELAY_MS), Some(50));
}
