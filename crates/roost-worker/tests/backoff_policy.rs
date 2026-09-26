//! The reconnect policy, tested as the pure function it is. Every constant here
//! came from an incident, and this is where the reasoning behind each is
//! pinned so the next person to "simplify" a number has to argue with it.

use std::time::Duration;

use roost_worker::backoff::{
    AUTH_REJECT_BACKOFF_CAP, AUTH_REJECT_THRESHOLD, AUTH_REJECT_THRESHOLD_AFTER_OPEN,
    BACKOFF_INITIAL, BACKOFF_MAX, LinkHealth, STABLE_SESSION, STALE_LINK_TIMEOUT, backoff_cap,
    backoff_delay,
};

/// THE 2026-08-01 INCIDENT. A worker throttled by its own cgroup escalated to
/// the five-minute cap after three dials and stayed invisible for minutes.
///
/// The fix is `has_opened`: a worker that has demonstrably worked is far more
/// likely to be meeting a transient stall than a rejected contract, so it gets
/// a much longer runway before escalation.
#[test]
fn a_worker_that_has_never_opened_escalates_quickly() {
    assert_eq!(backoff_cap(0, false), BACKOFF_MAX);
    assert_eq!(backoff_cap(AUTH_REJECT_THRESHOLD - 1, false), BACKOFF_MAX);
    assert_eq!(
        backoff_cap(AUTH_REJECT_THRESHOLD, false),
        AUTH_REJECT_BACKOFF_CAP,
        "a worker that has NEVER opened is probably carrying something the \\
         coordinator will not accept, and the honest response is to stop dialling \\
         hard enough that an operator notices"
    );
}

/// And the other half: a worker that has opened is given a long runway, because
/// a thirty-second coordinator blip must not become five minutes of silence.
#[test]
fn a_worker_that_has_opened_is_given_a_long_runway() {
    assert_eq!(
        backoff_cap(AUTH_REJECT_THRESHOLD, true),
        BACKOFF_MAX,
        "three failures are nothing for a worker that has demonstrably worked"
    );
    assert_eq!(
        backoff_cap(AUTH_REJECT_THRESHOLD_AFTER_OPEN - 1, true),
        BACKOFF_MAX
    );
    assert_eq!(
        backoff_cap(AUTH_REJECT_THRESHOLD_AFTER_OPEN, true),
        AUTH_REJECT_BACKOFF_CAP
    );
}

/// The two thresholds are far apart on purpose, and the asymmetry IS the fix
/// for 2026-08-01.
///
/// The values are recorded rather than compared: they are decisions, not
/// derived quantities, and a test that compared two constants would only
/// restate them.
#[test]
fn the_escalation_thresholds_are_far_apart() {
    assert_eq!(
        AUTH_REJECT_THRESHOLD, 3,
        "a worker that never opened gets three dials"
    );
    assert_eq!(
        AUTH_REJECT_THRESHOLD_AFTER_OPEN, 60,
        "a worker that has worked gets twenty times the runway, so a \
         thirty-second coordinator blip is never five minutes of silence"
    );
}

/// Backoff grows geometrically and saturates at the cap, so a short blip
/// retries fast and a long one stops hammering.
#[test]
fn backoff_grows_geometrically_and_saturates() {
    assert_eq!(backoff_delay(1, 0, true), BACKOFF_INITIAL);
    assert_eq!(backoff_delay(2, 0, true), BACKOFF_INITIAL * 2);
    assert_eq!(backoff_delay(3, 0, true), BACKOFF_INITIAL * 4);
    assert_eq!(
        backoff_delay(20, 0, true),
        BACKOFF_MAX,
        "and stops at the ceiling"
    );
}

/// Backoff must NEVER shorten. An attempt count large enough to overflow a
/// shift would produce a sub-second delay, and that is the one outcome a
/// reconnect loop must not have.
#[test]
fn a_huge_attempt_count_never_shortens_the_delay() {
    let normal = backoff_delay(10, 0, true);
    for attempt in [31, 32, 33, 100, u32::MAX] {
        let delay = backoff_delay(attempt, 0, true);
        assert!(
            delay >= normal,
            "attempt {attempt} produced {delay:?}, shorter than attempt 10's {normal:?}"
        );
        assert!(delay <= BACKOFF_MAX, "and never exceeds the cap: {delay:?}");
    }
}

/// The first attempt is fast. A coordinator restart is common and the worker
/// should find it quickly, not spend the first half-minute asleep.
#[test]
fn the_first_attempt_is_fast() {
    assert_eq!(backoff_delay(1, 0, true), BACKOFF_INITIAL);
    assert!(BACKOFF_INITIAL < BACKOFF_MAX);
}

/// The escalation raises the CEILING, not the floor. A worker that has just
/// escalated still retries quickly at first — it is the ceiling that moves, so
/// a recovery is noticed promptly.
#[test]
fn escalation_raises_the_ceiling_and_not_the_first_delay() {
    assert_eq!(
        backoff_delay(1, AUTH_REJECT_THRESHOLD, false),
        backoff_delay(1, 0, false),
        "the first dial after escalation is as fast as ever"
    );
    assert_eq!(
        backoff_delay(30, AUTH_REJECT_THRESHOLD, false),
        AUTH_REJECT_BACKOFF_CAP,
        "but the ceiling is now five minutes"
    );
}

/// THE STALE COUNTER must reset only after a link has stayed up long enough to
/// count as working. A link that opens and immediately drops has proved nothing.
#[test]
fn counters_reset_only_after_a_link_stays_up() {
    let mut health = LinkHealth::opened(Duration::from_secs(5));
    assert!(
        !health.should_reset_counters(),
        "five seconds is not a working session"
    );

    health.uptime = Some(STABLE_SESSION);
    assert!(health.should_reset_counters());
    health.uptime = Some(STABLE_SESSION + Duration::from_secs(1));
    assert!(health.should_reset_counters());
}

/// A coordinator flapping every few seconds must not be able to cycle the
/// attempt counter back to 1 forever, which would hide the pathology from
/// every dashboard reading it.
#[test]
fn a_flapping_coordinator_does_not_cycle_the_attempt_counter() {
    let mut health = LinkHealth::opened(Duration::from_secs(1));
    for _ in 0..20 {
        health.record_non_open();
    }
    assert_eq!(
        health.attempt, 21,
        "a link that never stayed up earns no reset"
    );
    assert_eq!(health.non_open_streak, 20);
}

/// A link that DID stay up, and then dropped, does get its counters back —
/// otherwise one early blip would cost the worker its full runway forever.
#[test]
fn a_link_that_worked_resets_the_streak() {
    let mut health = LinkHealth::opened(STABLE_SESSION);
    assert!(health.should_reset_counters());
    health.record_non_open();
    health.record_non_open();
    // A caller honouring `should_reset_counters` clears the streak here.
    health.non_open_streak = 0;
    health.attempt = 1;
    assert_eq!(health.next_delay(), BACKOFF_INITIAL);
}

/// THE 2026-07-11 INCIDENT. The coordinator died behind its front door; the
/// worker-side connection stayed ESTABLISHED and every send "succeeded" into a
/// black hole — no error, no close. That ran for seven hours.
#[test]
fn a_silent_link_is_stale_rather_than_idle() {
    let mut health = LinkHealth::opened(Duration::from_secs(600));
    health.since_last_frame = STALE_LINK_TIMEOUT - Duration::from_secs(1);
    assert!(
        !health.is_stale(),
        "just inside the window, it is merely quiet"
    );

    health.since_last_frame = STALE_LINK_TIMEOUT;
    assert!(
        health.is_stale(),
        "a link silent for a whole keepalive interval has had three pings \\
         missed, and must be force-closed and re-dialled"
    );
}

/// A link that never opened is not "stale" — there is nothing to close. The
/// distinction matters because a stale verdict on a closed link is a reconnect
/// loop.
#[test]
fn a_link_that_never_opened_is_not_stale() {
    let health = LinkHealth::new();
    health_never_opened(&health);
    assert!(!health.is_stale(), "there is no open link to be stale");
}

fn health_never_opened(health: &LinkHealth) {
    assert!(!health.has_opened);
    assert!(health.uptime.is_none());
}

/// A link with no traffic recorded is not stale, because a missing observation
/// is not evidence of silence. Treating it as evidence is how a healthy link
/// gets closed by its own watchdog.
#[test]
fn an_unobserved_link_is_not_declared_stale() {
    let health = LinkHealth::opened(Duration::from_secs(600));
    assert!(
        !health.is_stale(),
        "a link whose traffic has not been sampled is quiet, not dead"
    );
}

/// The stale window is a multiple of the keepalive, so "no traffic" means the
/// coordinator has missed a known number of pings rather than some arbitrary
/// silence.
#[test]
fn the_stale_window_is_a_whole_number_of_keepalive_intervals() {
    assert_eq!(
        STALE_LINK_TIMEOUT.as_secs() % 30,
        0,
        "the coordinator pings every 30s, so the window should be a whole \\
         number of those intervals"
    );
    assert!(
        STALE_LINK_TIMEOUT.as_secs() >= 90,
        "at least three missed pings"
    );
}
