//! The authority facts a pairing approval depends on: which account a device
//! belongs to, whether an account may still act, whether a key is revoked, and
//! whether a key is already a machine.
//!
//! Owned by the pairing slice. Ported from the account, device and revocation
//! reads in `apps/coord/src/auth/pairing-account.ts:260-347`.
//!
//! WHY THESE FOUR FACTS ARE RE-READ INSTEAD OF TRUSTED. An approval is a
//! decision taken up to ten minutes ago about the account, the key and the
//! approver as they were then. The confirmation that spends it must re-read all
//! three, because the entire question "may this requester become a device" is
//! answered by the world at the moment it is answered. A pairing module that
//! read `approved_account_id` and believed it would authorize a key into an
//! account that had been disabled while the code was being read aloud.
//!
//! A KEY IS NEVER BOTH A WORKER AND A DEVICE. `auth::principal` refuses such a
//! fingerprint at resolution time, so the pairing path refuses it earlier and
//! with a reason an operator can act on.

use super::PairingResult;

use super::{PairingRefusal, refuse};
use crate::db::CoordDb;

/// The account a new device would belong to, or `None` when none can be named.
///
/// Prefers the approver's own device row, and falls back to the single active
/// account. The fallback is what makes a first-run coordinator -- one account,
/// no devices yet -- pair at all; it is refused the moment there is a second
/// account, because from then on "the only account" is a guess, and pairing
/// into the wrong one is the failure this whole domain exists to prevent.
pub async fn paired_browser_account_id(
    database: &CoordDb,
    authority_fingerprint: Option<&str>,
) -> PairingResult<Option<String>> {
    if let Some(fingerprint) = authority_fingerprint {
        if let Some(account_id) = account_of_device(database.pool(), fingerprint).await? {
            return Ok(Some(account_id));
        }
    }
    Ok(single_active_account(database.pool()).await?)
}

/// Whether an approval's authority is still authority now.
///
/// Four facts, each re-read at the point of use: the account is still active,
/// the approving key is still a device of that account, and it is not revoked.
/// An approval made on a host with no account behind it
/// (`approved_by_fp IS NULL`) is valid exactly as long as the account is
/// active, because there is no key whose standing could have changed.
pub async fn pairing_approval_remains_valid(
    database: &CoordDb,
    approved_by_fingerprint: Option<&str>,
    approved_account_id: Option<&str>,
) -> PairingResult<bool> {
    let Some(account_id) = approved_account_id else {
        return Ok(false);
    };
    if !account_is_active(database.pool(), account_id).await? {
        return Ok(false);
    }
    let Some(fingerprint) = approved_by_fingerprint else {
        return Ok(true);
    };
    let device_account = account_of_device(database.pool(), fingerprint).await?;
    let revoked = is_revoked(database.pool(), fingerprint).await?;
    Ok(!revoked && device_account.as_deref() == Some(account_id))
}

/// Why a confirmed key cannot become this account's device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssociationConflict {
    /// The key is a worker's identity. A key that is both would carry two
    /// kinds of authority, which `auth::principal` refuses at resolution; this
    /// refuses it earlier, where the operator can read why.
    WorkerKey,
    /// The key already belongs to a different account.
    OtherAccount,
}

/// The conflict a confirmation would hit, or `None`.
pub async fn paired_browser_association_conflict(
    database: &CoordDb,
    fingerprint: &str,
    account_id: &str,
) -> PairingResult<Option<AssociationConflict>> {
    if worker_exists(database.pool(), fingerprint).await? {
        return Ok(Some(AssociationConflict::WorkerKey));
    }
    Ok(
        match account_of_device(database.pool(), fingerprint).await? {
            Some(existing) if existing != account_id => Some(AssociationConflict::OtherAccount),
            _ => None,
        },
    )
}

/// Make a confirmed fingerprint an account device, or refuse the conflict.
///
/// The insert upserts `last_seen_at_ms` and not `account_id`: re-pairing a key
/// that is already this account's device must refresh it without ever moving it
/// between accounts, so the column that names the account is not in the
/// `DO UPDATE SET` at all.
pub async fn associate_paired_browser(
    database: &CoordDb,
    fingerprint: &str,
    account_id: &str,
    now_ms: i64,
) -> PairingResult<()> {
    match paired_browser_association_conflict(database, fingerprint, account_id).await? {
        Some(AssociationConflict::WorkerKey) => {
            return Err(refuse(PairingRefusal::WorkerKeyConflict));
        }
        Some(AssociationConflict::OtherAccount) => {
            return Err(refuse(PairingRefusal::OtherAccountConflict));
        }
        None => {}
    }
    sqlx::query(
        "INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT (fingerprint) DO UPDATE SET last_seen_at_ms = excluded.last_seen_at_ms",
    )
    .bind(fingerprint)
    .bind(account_id)
    .bind(now_ms)
    .bind(now_ms)
    .execute(database.pool())
    .await
    .map_err(|error| super::sqlx_error("pairing.associate", error))?;
    Ok(())
}

/// The account a device fingerprint belongs to.
pub async fn account_of_device<'a, E>(
    executor: E,
    fingerprint: &str,
) -> PairingResult<Option<String>>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    one_string(
        executor,
        "SELECT account_id FROM account_devices WHERE fingerprint = ?",
        fingerprint,
    )
    .await
}

/// Whether an account can still act for a device.
pub async fn account_is_active<'a, E>(executor: E, account_id: &str) -> PairingResult<bool>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    let row = sqlx::query_as::<_, (i64,)>(
        "SELECT COUNT(*) FROM accounts WHERE id = ? AND status = 'active'",
    )
    .bind(account_id)
    .fetch_one(executor)
    .await
    .map_err(|error| super::sqlx_error("pairing.account_status", error))?;
    Ok(row.0 == 1)
}

/// The one active account, when there is exactly one.
pub async fn single_active_account<'a, E>(executor: E) -> PairingResult<Option<String>>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    // `LIMIT 2`, not `LIMIT 1`: the question is "is there exactly one", and a
    // query that cannot see the second account cannot answer it.
    let rows =
        sqlx::query_as::<_, (String,)>("SELECT id FROM accounts WHERE status = 'active' LIMIT 2")
            .fetch_all(executor)
            .await
            .map_err(|error| super::sqlx_error("pairing.account", error))?;
    Ok(match rows.as_slice() {
        [(id,)] => Some(id.clone()),
        _ => None,
    })
}

/// Whether a fingerprint is on the revocation list.
pub async fn is_revoked<'a, E>(executor: E, fingerprint: &str) -> PairingResult<bool>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    let row = sqlx::query_as::<_, (i64,)>(
        "SELECT COUNT(*) FROM authorized_key_revocations WHERE fingerprint = ?",
    )
    .bind(fingerprint)
    .fetch_one(executor)
    .await
    .map_err(|error| super::sqlx_error("pairing.revocation", error))?;
    Ok(row.0 == 1)
}

/// Whether a fingerprint is a registered worker's identity.
///
/// A tombstoned worker still has a row (`auth::authorized_keys`), so this is a
/// row-existence question and not a liveness one: a key that ever identified
/// a machine must not silently become a browser.
pub async fn worker_exists<'a, E>(executor: E, fingerprint: &str) -> PairingResult<bool>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    let row = sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM workers WHERE fp = ?")
        .bind(fingerprint)
        .fetch_one(executor)
        .await
        .map_err(|error| super::sqlx_error("pairing.worker", error))?;
    Ok(row.0 == 1)
}

/// One string column, looked up by a text key.
async fn one_string<'a, E>(
    executor: E,
    statement: &'static str,
    key: &str,
) -> PairingResult<Option<String>>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    sqlx::query_as::<_, (String,)>(statement)
        .bind(key)
        .fetch_optional(executor)
        .await
        .map(|row| row.map(|(value,)| value))
        .map_err(|error| super::sqlx_error("pairing.read_column", error))
}
