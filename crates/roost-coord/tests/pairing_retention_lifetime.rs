//! The retention sweep's LIFETIME, as distinct from its behaviour.
//!
//! Owned by the pairing slice. `pairing_retention.rs` asserts what a sweep does
//! to rows; this file asserts WHEN it happens and that it can be stopped, and
//! those are the two properties that make it a retention policy rather than a
//! function nobody calls.
//!
//! WHY THE PRE-SLEEP SWEEP IS THE ONE THAT MATTERS. A live pair request past
//! its deadline is a credential until something notices: anybody holding the id
//! and the requester token can be confirmed into a device. "A minute later" is
//! the whole window, so a coordinator that was down over a deadline, or one that
//! starts with a backlog, must reclaim at boot rather than on its first tick.
//! `PAIR_REQUEST_SWEEP_INTERVAL_MS` is 60 000, so a test that waits two seconds
//! and asserts the row is expired cannot be satisfied by a loop that sleeps
//! first -- that is what makes this a discriminator rather than a timing
//! assertion dressed up as one.
//!
//! THE SHUTDOWN HALF MATTERS EQUALLY. A spawned task that cannot be stopped is
//! a leak with a name, and the sweep holds a database handle and a bus
//! publisher for as long as it runs. `stop()` is the only thing that waits the
//! in-flight tick out, so the tests assert it RETURNS within a budget rather
//! than assuming it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use roost_coord::auth::pairing::retention::{
    PAIR_REQUEST_SWEEP_INTERVAL_MS, spawn_pair_request_retention,
};
use roost_coord::db::CoordDb;
use roost_coord::services::CoordServices;
use sqlx::AssertSqlSafe;

/// Comfortably below the sweep interval, so a pre-sleep sweep is the only way
/// this can pass. Two seconds against sixty.
const OBSERVE_WINDOW: Duration = Duration::from_secs(2);

/// Comfortably below the sweep's own five-second shutdown grace, so a `stop()`
/// that hangs fails here rather than at the grace.
const STOP_BUDGET: Duration = Duration::from_secs(4);

/// A deadline in 2023, which is overdue against whatever the wall clock says
/// when this runs. The sweep reads the real clock, so the seed cannot be
/// relative to the test.
const LONG_OVERDUE: i64 = 1_700_000_000_000;

const OVERDUE: &str = "00000000000000000000000000000001";
const STILL_LIVE: &str = "00000000000000000000000000000002";

struct SweepFixture {
    services: Arc<CoordServices>,
    root: PathBuf,
}

impl SweepFixture {
    async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!("roost-pairing-sweep-{label}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database: CoordDb = db_support::open_test_database(&root)
            .await
            .expect("a migrated database");
        let fixture = Self {
            services: Arc::new(CoordServices::new(database)),
            root,
        };
        fixture.seed().await;
        fixture
    }

    /// One request long past its deadline and one with no deadline in sight: the
    /// pair that separates "the sweep ran" from "the sweep expired everything".
    async fn seed(&self) {
        for (handle, expires_at_ms) in [(OVERDUE, LONG_OVERDUE), (STILL_LIVE, i64::MAX)] {
            sqlx::query(AssertSqlSafe(format!(
                "INSERT INTO pair_requests ( \
                     id, ephemeral_id, public_key, label, status, created_at_ms, decided_at_ms, \
                     ceremony_version, requester_token_hash, verification_code_hash, \
                     verification_attempts, expires_at_ms) \
                 VALUES ('row-{handle}', '{handle}', $1, 'laptop', 'pending', 0, NULL, \
                         1, 'token-digest', NULL, 0, {expires_at_ms})"
            )))
            .bind(vec![1_u8, 2])
            .execute(self.services.db.pool())
            .await
            .expect("a seed statement to apply");
        }
    }

    async fn status_of(&self, handle: &str) -> Option<String> {
        sqlx::query_as::<_, (String,)>("SELECT status FROM pair_requests WHERE ephemeral_id = $1")
            .bind(handle)
            .fetch_optional(self.services.db.pool())
            .await
            .expect("a status read")
            .map(|(status,)| status)
    }
}

impl Drop for SweepFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Poll `read` until it answers true, or fail naming what was still false.
async fn eventually<F, Fut>(what: &str, mut read: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + OBSERVE_WINDOW;
    loop {
        if read().await {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what} did not happen within {OBSERVE_WINDOW:?}. The sweep interval is \
             {PAIR_REQUEST_SWEEP_INTERVAL_MS}ms, so a window this short can only be \
             satisfied by a sweep that runs BEFORE its first sleep."
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// THE PROPERTY. The sweep runs once at spawn, before it ever sleeps, so a
/// coordinator that starts with an overdue request reclaims it immediately
/// rather than a minute later.
///
/// Asserted by DEADLINE, not by observing that the row eventually changes: the
/// observation window is two seconds and the interval is sixty, so a loop that
/// slept first could not satisfy it at all.
#[tokio::test]
async fn the_sweep_runs_before_its_first_sleep() {
    let fixture = SweepFixture::new("pre-sleep").await;
    assert_eq!(
        fixture.status_of(OVERDUE).await.as_deref(),
        Some("pending"),
        "the request starts live, or the test proves nothing"
    );

    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let sweep = spawn_pair_request_retention(Arc::clone(&fixture.services), receiver);

    eventually("an overdue request expiring at spawn", || async {
        fixture.status_of(OVERDUE).await.as_deref() == Some("expired")
    })
    .await;

    // And the sweep is SELECTIVE. A request with no deadline in sight survives
    // the same pass, which is what makes this evidence of a real sweep rather
    // than of a blanket status write.
    assert_eq!(
        fixture.status_of(STILL_LIVE).await.as_deref(),
        Some("pending"),
        "a request inside its window must survive the pre-sleep sweep"
    );

    drop(shutdown);
    tokio::time::timeout(STOP_BUDGET, sweep.stop())
        .await
        .expect("the sweep must stop when its shutdown sender is dropped");
}

/// Dropping the sender stops the sweep, which is the forgiving half of the
/// shutdown contract. It is a real property rather than an accident of
/// `watch`: `changed()` returns `Err` once every sender is gone, and a
/// coordinator whose shutdown path forgets to send a value would hang forever
/// without it.
#[tokio::test]
async fn the_sweep_stops_when_its_shutdown_signal_fires() {
    let fixture = SweepFixture::new("signal").await;
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let sweep = spawn_pair_request_retention(Arc::clone(&fixture.services), receiver);

    shutdown
        .send(true)
        .expect("the sweep holds the receiving half");
    tokio::time::timeout(STOP_BUDGET, sweep.stop())
        .await
        .expect("the sweep must stop when its shutdown signal is sent");
}

/// The interval is a minute and a request lives ten. Both are constants a
/// reader could plausibly tidy, and the ratio is the load-bearing part: the
/// sweep must run more often than a request lives, or an expired request could
/// outlive the next sweep -- which is the window in which a dead request is
/// still a credential.
#[tokio::test]
async fn the_sweep_runs_more_often_than_a_request_lives() {
    use roost_coord::auth::pairing::secrets::PAIR_REQUEST_TTL_MS;

    assert_eq!(PAIR_REQUEST_SWEEP_INTERVAL_MS, 60_000);
    assert_eq!(
        PAIR_REQUEST_TTL_MS, 600_000,
        "a request is redeemable for ten minutes"
    );
    assert!(
        PAIR_REQUEST_SWEEP_INTERVAL_MS < PAIR_REQUEST_TTL_MS as u64,
        "an expired request must not be able to outlive the next sweep"
    );
}
