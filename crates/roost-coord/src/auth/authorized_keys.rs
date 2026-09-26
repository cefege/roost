//! Resolving a `kid` to an authorized key and then to a principal, over the
//! two tables and the rules that decide them.
//!
//! Owned by the coordinator's auth layer. This is the only place a bearer token
//! meets the database, so it is the only place the `authorized_keys ->
//! account_devices -> accounts` and `authorized_keys -> workers` joins live.
//!
//! WHY THE JOINS ARE SEPARATE QUERIES AND NOT ONE. `resolveCallerPrincipal`
//! left-joins both and then refuses a key that has BOTH
//! (`apps/coord/src/auth/auth-principal.ts:66`): "A key must never acquire two
//! kinds of authority." Two queries and a rule in Rust make that refusal
//! explicit; one combined query makes it a column that is quietly `NULL` for
//! half the rows, and the difference is invisible until a key is somehow both.
//!
//! WHY A TOMBSTONED WORKER IS REFUSED RATHER THAN DELETED. `workers` keeps a
//! `deleted_at_ms` because a deleted worker's sessions are still referenced by
//! rows that must survive it (`apps/coord/migrations/0025_worker_tombstones.sql`).
//! A tombstoned worker therefore still has a row, and the row is not authority.

use crate::auth::jwt_crypto::PublicKey;
use crate::auth::principal::{Principal, PrincipalFacts, resolve_principal};
use crate::db::CoordDb;
use roost_protocol::{ProtocolError, ProtocolResult};

/// The raw `authorized_keys` row a `kid` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedKey {
    /// The 32 raw bytes of the Ed25519 public key.
    pub public_key: Vec<u8>,
    /// The operator-facing label.
    pub label: String,
}

/// Read the authorized key a `kid` names.
///
/// `None` is not an error: an unknown `kid` is the single most common refusal
/// and it must not read as a database fault in a log.
pub async fn load_authorized_key(
    database: &CoordDb,
    kid: &str,
) -> Result<Option<AuthorizedKey>, ProtocolError> {
    let row = sqlx::query_as::<_, (Vec<u8>, String)>(
        "SELECT public_key, label FROM authorized_keys WHERE fingerprint = ?",
    )
    .bind(kid)
    .fetch_optional(database.pool())
    .await
    .map_err(sqlx_error)?;
    Ok(row.map(|(public_key, label)| AuthorizedKey { public_key, label }))
}

/// Read the account-device facts for a key.
pub async fn load_account_device_facts(
    database: &CoordDb,
    kid: &str,
) -> Result<(bool, Option<String>, Option<String>), ProtocolError> {
    let row = sqlx::query_as::<_, (Option<String>, Option<String>)>(
        "SELECT device.account_id, account.status
           FROM account_devices AS device
           JOIN accounts AS account ON account.id = device.account_id
          WHERE device.fingerprint = ?",
    )
    .bind(kid)
    .fetch_optional(database.pool())
    .await
    .map_err(sqlx_error)?;
    Ok(match row {
        Some((account_id, status)) => (true, account_id, status),
        None => (false, None, None),
    })
}

/// Read the worker facts for a key.
///
/// `tombstoned` is `workers.deleted_at_ms IS NOT NULL`, folded here so the
/// caller's rule set reads as one boolean rather than a nullable column it might
/// compare wrongly.
pub async fn load_worker_facts(
    database: &CoordDb,
    kid: &str,
) -> Result<(bool, bool), ProtocolError> {
    let row =
        sqlx::query_as::<_, (bool,)>("SELECT deleted_at_ms IS NOT NULL FROM workers WHERE fp = ?")
            .bind(kid)
            .fetch_optional(database.pool())
            .await
            .map_err(sqlx_error)?;
    Ok(match row {
        Some((tombstoned,)) => (true, tombstoned),
        None => (false, false),
    })
}

/// Read every fact a principal decision needs, then decide.
///
/// One call so the caller cannot read the key table and skip the principal rules
/// -- the rules are the security property, and a two-call shape is a two-call
/// shape somebody eventually gets wrong.
pub async fn resolve_key_principal(
    database: &CoordDb,
    kid: &str,
) -> Result<Option<Principal>, ProtocolError> {
    let Some(key) = load_authorized_key(database, kid).await? else {
        return Ok(None);
    };
    let (account_device, account_id, account_status) =
        load_account_device_facts(database, kid).await?;
    let (worker, worker_tombstoned) = load_worker_facts(database, kid).await?;
    let facts = PrincipalFacts {
        authorized_key: true,
        account_device,
        account_status,
        account_id,
        worker,
        worker_tombstoned,
        label: key.label,
    };
    // A refusal is `None`, not an error: every refusal in `resolve_principal`
    // means "this key is not a principal right now", and the transports all
    // answer 401 for it regardless of which rule fired.
    Ok(resolve_principal(kid, &facts).ok())
}

/// The `PublicKey` a stored row names.
///
/// A row of the wrong length is an error rather than a skipped key: a truncated
/// `public_key` is a database fault, and silently treating it as "no such key"
/// would turn a repairable migration bug into an authentication outage nobody can
/// see the cause of.
pub fn public_key_of(key: &AuthorizedKey) -> ProtocolResult<PublicKey> {
    PublicKey::from_bytes(&key.public_key)
        .map_err(|failure| ProtocolError::new("auth.authorized_keys", failure.to_string()))
}

/// The fingerprint of a raw public key, rendered by the ONE renderer the
/// workspace owns.
///
/// `roost_protocol::fingerprint`'s header states why there is exactly one: "All
/// three ends of the protocol derive a worker's, a coordinator's, or a device's
/// fingerprint independently, so a byte-for-byte divergence here silently breaks
/// pairing, JWT `kid` lookup, and authorized-keys matching at once."
#[must_use]
pub fn fingerprint_of_raw_public_key(public_key: &[u8; 32]) -> String {
    use sha2::Digest as _;
    let digest: [u8; 32] = sha2::Sha256::digest(public_key).into();
    roost_protocol::fingerprint::fingerprint_hex(&digest)
}

fn sqlx_error(error: sqlx::Error) -> ProtocolError {
    ProtocolError::new("auth.sqlite", error.to_string())
}
