//! The retention window itself: `ts` is epoch milliseconds, and the boundary
//! is strictly older-than rather than at-or-older.
//!
//! A window computed in seconds is a window ninety days long measured against
//! timestamps a thousand times too large, so it deletes nothing and the table
//! regrows exactly as it did before the sweep existed. The test pins the unit by
//! seeding one row at each scale and asserting which of them go.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod maintenance_audit_support;

use maintenance_audit_support::{
    AuditFixture, NOW_MS, RETENTION_DAYS, SESSIONS_INPUT_PATH, days_ago,
};
use roost_coord::maintenance::audit_retention::DAY_MS;

/// The cutoff this fixture's sweep computes: ninety whole days before `NOW_MS`.
fn cutoff_ms() -> i64 {
    NOW_MS - i64::try_from(RETENTION_DAYS).expect("90 days fits in an i64") * DAY_MS
}

#[tokio::test]
async fn the_window_is_epoch_milliseconds_not_seconds() {
    let fixture = AuditFixture::new("milliseconds").await;
    // A seconds-scale timestamp for "now". A sweep computed in seconds would
    // leave this row alone, because it would be exactly at its own cutoff.
    let seconds_scale = fixture
        .seed(NOW_MS / 1000, "POST", SESSIONS_INPUT_PATH)
        .await;
    // A millisecond-scale row a second old.
    let fresh = fixture
        .seed(NOW_MS - 1000, "POST", SESSIONS_INPUT_PATH)
        .await;
    // Exactly at the cutoff, and a millisecond inside it.
    let at_cutoff = fixture.seed(cutoff_ms(), "POST", SESSIONS_INPUT_PATH).await;
    let inside = fixture
        .seed(cutoff_ms() - 1, "POST", SESSIONS_INPUT_PATH)
        .await;

    assert_eq!(fixture.sweep().await, 2);
    let kept = fixture.surviving_ids().await;
    assert_eq!(
        kept,
        vec![fresh, at_cutoff],
        "a row at the cutoff is kept: the comparison is strictly older-than"
    );
    assert!(
        !kept.contains(&seconds_scale) && !kept.contains(&inside),
        "a seconds-scale timestamp is ancient to a millisecond window, and a \
         millisecond inside the window is swept: {kept:?}"
    );
}

#[test]
fn a_day_is_the_same_day_the_window_and_the_backup_schedule_count_in() {
    assert_eq!(DAY_MS, 24 * 60 * 60 * 1000);
    assert_eq!(cutoff_ms(), days_ago(RETENTION_DAYS as i64));
}
