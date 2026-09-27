//! Retiring one key: the revocation row, the grants that die with it, and every
//! row that still names it.
//!
//! This is the one sequence in the auth layer that must not be reordered, split
//! or partly applied, so it has its own file and one name. Three callers reach
//! it — `DevicesRevoke`, `DevicesRotateCurrent` and `AuthLogout` — and the only
//! thing that distinguishes them is who revoked what and how wide the sweep of
//! unclaimed grants is. A fourth caller that needs a different shape is a sign
//! this wants a different sequence, not a new boolean.
//!
//! WHY THE ORDER IS WHAT IT IS. The revocation row goes FIRST because it is the
//! only statement that stops a verifier resolving the key: delete the
//! `authorized_keys` row first and a token minted a moment earlier still
//! resolves, because the row is what a `kid` is looked up in. Every delete after
//! it removes rows that would otherwise keep naming a fingerprint nothing can
//! use any more.

use connectrpc::ConnectError;
use sqlx::{Sqlite, Transaction};

use crate::auth::db_statements::{Bind, run};

/// The one sequence that retires a key, in the order that is safe if it stops
/// half way. The caller holds the transaction, so a failure anywhere above
/// rolls back the revocation with it.
pub(crate) async fn retire_principal(
    transaction: &mut Transaction<'_, Sqlite>,
    fingerprint: &str,
    revoked_by: &str,
    reason: &str,
    sweep_host_grants: bool,
    account_id: Option<&str>,
    now: i64,
) -> Result<(), ConnectError> {
    run(
        transaction,
        "INSERT INTO authorized_key_revocations (fingerprint, revoked_at_ms, revoked_by_fp, \
         reason) VALUES (?, ?, ?, ?)",
        &[
            Bind::Text(Some(fingerprint)),
            Bind::Int(now),
            Bind::Text(Some(revoked_by)),
            Bind::Text(Some(reason)),
        ],
    )
    .await?;
    // Both sweeps are one statement each, with the branch folded into the
    // predicate rather than into a second copy of the SQL: a sweep whose two
    // arms drift apart is a sweep whose difference nobody can see.
    run(
        transaction,
        "DELETE FROM bootstrap_tokens WHERE used_at_ms IS NULL \
         AND (minted_by_fp = ? OR (? <> 0 AND minted_by_fp IS NULL))",
        &[
            Bind::Text(Some(fingerprint)),
            Bind::Int(i64::from(sweep_host_grants)),
        ],
    )
    .await?;
    run(
        transaction,
        "DELETE FROM push_subscriptions WHERE viewer_fp = ?",
        &[Bind::Text(Some(fingerprint))],
    )
    .await?;
    run(
        transaction,
        "DELETE FROM account_devices WHERE fingerprint = ? \
         AND (? IS NULL OR account_id = ?)",
        &[
            Bind::Text(Some(fingerprint)),
            Bind::Text(account_id),
            Bind::Text(account_id),
        ],
    )
    .await?;
    run(
        transaction,
        "DELETE FROM authorized_keys WHERE fingerprint = ?",
        &[Bind::Text(Some(fingerprint))],
    )
    .await
}
