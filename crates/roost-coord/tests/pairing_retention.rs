//! Pair-request retention's two ends, against a real database.
//!
//! The sweep is the only thing standing between a decided pair request and
//! forever, and the only thing standing between an overdue one and a standing
//! credential. Both ends are guarded here, because "expire the live rows" and
//! "reclaim the dead rows" are separate statements and a bug in one is silent
//! in the other.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use roost_coord::auth::pairing::retention::{PAIR_REQUEST_TOMBSTONE_MS, sweep_pair_requests};
use roost_coord::db::CoordDb;
use sqlx::AssertSqlSafe;

const NOW: i64 = 1_700_000_000_000;
const OVERDUE_PENDING: &str = "00000000000000000000000000000001";
const OVERDUE_VERIFYING: &str = "00000000000000000000000000000002";
const LIVE_PENDING: &str = "00000000000000000000000000000003";
const OLD_TOMBSTONE: &str = "00000000000000000000000000000004";
const FRESH_TOMBSTONE: &str = "00000000000000000000000000000005";

struct RetentionFixture {
    database: CoordDb,
    root: PathBuf,
}

impl RetentionFixture {
    async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!("roost-pairing-retention-{label}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated database");
        let fixture = Self { database, root };
        fixture.seed().await;
        fixture
    }

    /// Five requests that between them cover every branch of the sweep: two
    /// live rows past their deadline, one live row still redeemable, one
    /// decided row older than the tombstone window, and one decided row inside
    /// it.
    async fn seed(&self) {
        // EVERY insert is awaited. `insert` is async, so a bare call builds a
        // future and drops it: the fixture seeds nothing, and every assertion
        // below then fails on a row that was never there rather than on
        // anything about the sweep.
        self.insert(OVERDUE_PENDING, "pending", NOW - 1_000, Some("digest-a"), 0)
            .await;
        self.insert(
            OVERDUE_VERIFYING,
            "verification_required",
            NOW - 1_000,
            Some("digest-b"),
            2,
        )
        .await;
        self.insert(LIVE_PENDING, "pending", NOW + 600_000, None, 0)
            .await;
        self.insert(
            OLD_TOMBSTONE,
            "denied",
            NOW - PAIR_REQUEST_TOMBSTONE_MS - 1,
            None,
            0,
        )
        .await;
        self.insert(FRESH_TOMBSTONE, "completed", NOW - 1_000, None, 0)
            .await;
    }

    async fn insert(
        &self,
        ephemeral_id: &str,
        status: &str,
        expires_at_ms: i64,
        code_hash: Option<&str>,
        attempts: i64,
    ) {
        let decided_at = if status == "pending" || status == "verification_required" {
            "NULL"
        } else {
            &expires_at_ms.to_string()
        };
        self.exec(&format!(
            "INSERT INTO pair_requests ( \
                 id, ephemeral_id, public_key, label, status, created_at_ms, decided_at_ms, \
                 ceremony_version, requester_token_hash, verification_code_hash, \
                 verification_attempts, expires_at_ms) \
             VALUES ('row-{ephemeral_id}', '{ephemeral_id}', x'0102', 'laptop', '{status}', \
                     0, {decided_at}, 1, 'token-digest', \
                     {code_hash}, {attempts}, {expires_at_ms})",
            code_hash = match code_hash {
                Some(digest) => format!("'{digest}'"),
                None => "NULL".to_string(),
            }
        ))
        .await;
    }

    async fn exec(&self, statement: &str) {
        sqlx::query(AssertSqlSafe(statement))
            .execute(self.database.pool())
            .await
            .expect("a seed statement to apply");
    }

    async fn status_of(&self, ephemeral_id: &str) -> Option<String> {
        sqlx::query_as::<_, (String,)>("SELECT status FROM pair_requests WHERE ephemeral_id = ?")
            .bind(ephemeral_id)
            .fetch_optional(self.database.pool())
            .await
            .expect("a status read")
            .map(|(status,)| status)
    }

    async fn code_hash_of(&self, ephemeral_id: &str) -> Option<(Option<String>, i64)> {
        sqlx::query_as::<_, (Option<String>, i64)>(
            "SELECT verification_code_hash, verification_attempts FROM pair_requests \
              WHERE ephemeral_id = ?",
        )
        .bind(ephemeral_id)
        .fetch_optional(self.database.pool())
        .await
        .expect("a digest read")
        .map(|(hash, attempts)| (hash, attempts))
    }

    async fn exists(&self, ephemeral_id: &str) -> bool {
        self.status_of(ephemeral_id).await.is_some()
    }
}

impl Drop for RetentionFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The expire end: every live row past its deadline terminalizes, in both live
/// phases, and the one still inside its window is untouched.
#[tokio::test]
async fn the_sweep_expires_every_overdue_live_request() {
    let fixture = RetentionFixture::new("expire").await;
    let outcome = sweep_pair_requests(&fixture.database, NOW)
        .await
        .expect("a sweep");

    let mut expired = outcome.expired.clone();
    expired.sort();
    assert_eq!(
        expired,
        vec![OVERDUE_PENDING.to_string(), OVERDUE_VERIFYING.to_string()],
        "both overdue phases must terminalize, and the live one must not"
    );
    assert_eq!(
        fixture.status_of(OVERDUE_PENDING).await.as_deref(),
        Some("expired")
    );
    assert_eq!(
        fixture.status_of(OVERDUE_VERIFYING).await.as_deref(),
        Some("expired")
    );
    assert_eq!(
        fixture.status_of(LIVE_PENDING).await.as_deref(),
        Some("pending"),
        "a request inside its window is still redeemable"
    );
}

/// An expired request keeps NO code digest. A request that expired with its
/// bound code still attached is a request whose code can keep being matched
/// against a copy of the row, which is why the expiry statement and the clear
/// are one statement.
#[tokio::test]
async fn an_expired_request_keeps_no_code_digest() {
    let fixture = RetentionFixture::new("digest").await;
    sweep_pair_requests(&fixture.database, NOW)
        .await
        .expect("a sweep");

    for handle in [OVERDUE_PENDING, OVERDUE_VERIFYING] {
        let (hash, attempts) = fixture.code_hash_of(handle).await.expect("a surviving row");
        assert_eq!(
            hash, None,
            "{handle} expired and still carries a code digest"
        );
        assert!(
            attempts <= roost_coord::auth::pairing::secrets::PAIR_VERIFICATION_ATTEMPT_LIMIT,
            "{handle} must not carry an attempt count above the bound"
        );
    }
}

/// The reclaim end: a decided row past the tombstone window goes, and one
/// inside it stays -- because "when was this device authorised, and by whom"
/// is a question somebody asks long after the ceremony.
#[tokio::test]
async fn the_sweep_reclaims_only_tombstones_older_than_a_day() {
    let fixture = RetentionFixture::new("tombstone").await;
    let outcome = sweep_pair_requests(&fixture.database, NOW)
        .await
        .expect("a sweep");

    assert_eq!(outcome.deleted, 1, "exactly one row is past the window");
    assert!(
        !fixture.exists(OLD_TOMBSTONE).await,
        "a decided row past the window must be deleted"
    );
    assert!(
        fixture.exists(FRESH_TOMBSTONE).await,
        "a decided row inside the window is the forensic record and must stay"
    );
}

/// The sweep is idempotent: a second pass over the same instant finds nothing,
/// which is what makes a per-minute schedule safe rather than a source of
/// write-lock contention.
#[tokio::test]
async fn a_settled_sweep_reclaims_nothing_on_a_second_pass() {
    let fixture = RetentionFixture::new("idempotent").await;
    sweep_pair_requests(&fixture.database, NOW)
        .await
        .expect("a first sweep");
    let second = sweep_pair_requests(&fixture.database, NOW)
        .await
        .expect("a second sweep");

    assert!(second.expired.is_empty(), "nothing is overdue any more");
    assert_eq!(second.deleted, 0, "the tombstone is already gone");
    assert!(fixture.exists(FRESH_TOMBSTONE).await);
}
