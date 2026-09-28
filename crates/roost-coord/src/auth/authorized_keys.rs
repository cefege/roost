//! Resolving a `kid` to an authorized key and then to a principal, over the
//! two tables and the rules that decide them -- and importing the operator's
//! `authorized_keys` file into those tables at boot.
//!
//! Owned by the coordinator's auth layer; `serve` calls the import once, after
//! migrations and before the tenancy invariant. Ports
//! `apps/coord/src/auth/authorized-keys.ts`. This is the only place a bearer
//! token meets the database, so it is the only place the `authorized_keys ->
//! account_devices -> accounts` and `authorized_keys -> workers` joins live.
//!
//! **NO v2 DATABASE IS EVER READ FROM HERE.** The keys this module resolves
//! are the ones this install paired, and they arrive by `roost import-v2`
//! reading a v2 database read-only, once, before the first boot — never by the
//! coordinator opening one, and never by an auth path that had to understand a
//! file belonging to a product that is being replaced.
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

use std::path::Path;

use crate::auth::bootstrap_tokens::decode_ssh_wire_ed25519_pubkey;
use crate::auth::jwt_crypto::PublicKey;
use crate::auth::principal::{Principal, PrincipalFacts, resolve_principal};
use crate::db::CoordDb;
use roost_protocol::{ProtocolError, ProtocolResult};

/// One key an `authorized_keys` line names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAuthorizedKey {
    /// The 32 raw bytes of the Ed25519 public key.
    pub public_key: [u8; 32],
    /// Everything after the blob, or `(no label)` when the line had none.
    pub label: String,
}

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

/// Parse one `ssh-ed25519 <blob> [label…]` line; `None` for anything else.
///
/// Blank lines, `#` comments, other key types and undecodable blobs are all
/// skipped rather than refused, as v2's `parseSshEd25519Line` skips them: one
/// bad line in an operator's file must not cost every good one after it.
#[must_use]
pub fn parse_ssh_ed25519_line(line: &str) -> Option<ParsedAuthorizedKey> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let mut fields = trimmed.split_whitespace();
    if fields.next() != Some("ssh-ed25519") {
        return None;
    }
    let public_key = decode_ssh_wire_ed25519_pubkey(fields.next()?)?;
    let label = fields.collect::<Vec<_>>().join(" ");
    Some(ParsedAuthorizedKey {
        public_key,
        label: if label.is_empty() {
            "(no label)".to_string()
        } else {
            label
        },
    })
}

/// Upsert every key the file at `file_path` names; returns how many lines did.
///
/// v2's `importAuthorizedKeys`. A revoked fingerprint is skipped, never
/// resurrected. A re-listed key keeps its row and takes the file's label. When
/// the database holds exactly one account and it is active, each imported key
/// that is not a worker also becomes that account's device -- the one account
/// a self-hosted install has is the only owner the key can mean. The count is
/// of imported LINES, so a key listed twice counts twice, as in v2.
pub async fn import_authorized_keys(
    database: &CoordDb,
    file_path: &Path,
    now_ms: i64,
) -> Result<usize, ProtocolError> {
    let bytes = tokio::fs::read(file_path).await.map_err(|error| {
        ProtocolError::new(
            "auth.authorized_keys",
            format!("read {}: {error}", file_path.display()),
        )
    })?;
    let contents = String::from_utf8_lossy(&bytes);
    let accounts =
        sqlx::query_as::<_, (String, String)>("SELECT id, status FROM accounts LIMIT 2")
            .fetch_all(database.pool())
            .await
            .map_err(sqlx_error)?;
    let browser_account_id = match accounts.as_slice() {
        [(id, status)] if status == "active" => Some(id.clone()),
        _ => None,
    };
    let mut imported = 0_usize;
    for line in contents.lines() {
        let Some(parsed) = parse_ssh_ed25519_line(line) else {
            continue;
        };
        let fingerprint = fingerprint_of_raw_public_key(&parsed.public_key);
        if key_is_revoked(database, &fingerprint).await? {
            tracing::info!(fingerprint = %fingerprint, "authorized key import skipped: revoked");
            continue;
        }
        upsert_imported_key(
            database,
            &fingerprint,
            &parsed,
            browser_account_id.as_deref(),
            now_ms,
        )
        .await
        .map_err(sqlx_error)?;
        imported += 1;
    }
    Ok(imported)
}

async fn key_is_revoked(database: &CoordDb, fingerprint: &str) -> Result<bool, ProtocolError> {
    let row = sqlx::query_as::<_, (i64,)>(
        "SELECT 1 FROM authorized_key_revocations WHERE fingerprint = ?",
    )
    .bind(fingerprint)
    .fetch_optional(database.pool())
    .await
    .map_err(sqlx_error)?;
    Ok(row.is_some())
}

/// One line's writes, in one transaction so a key never lands without the
/// device row its account ownership implies.
async fn upsert_imported_key(
    database: &CoordDb,
    fingerprint: &str,
    parsed: &ParsedAuthorizedKey,
    browser_account_id: Option<&str>,
    now_ms: i64,
) -> Result<(), sqlx::Error> {
    let mut transaction = database.pool().begin().await?;
    sqlx::query(
        "INSERT INTO authorized_keys (fingerprint, public_key, label, added_at) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT (fingerprint) DO UPDATE SET label = excluded.label",
    )
    .bind(fingerprint)
    .bind(parsed.public_key.as_slice())
    .bind(&parsed.label)
    .bind(now_ms)
    .execute(&mut *transaction)
    .await?;
    if let Some(account_id) = browser_account_id {
        let worker = sqlx::query_as::<_, (i64,)>("SELECT 1 FROM workers WHERE fp = ?")
            .bind(fingerprint)
            .fetch_optional(&mut *transaction)
            .await?;
        if worker.is_none() {
            sqlx::query(
                "INSERT INTO account_devices \
                   (fingerprint, account_id, added_at_ms, last_seen_at_ms) \
                 VALUES (?, ?, ?, ?) \
                 ON CONFLICT (fingerprint) DO UPDATE SET last_seen_at_ms = excluded.last_seen_at_ms",
            )
            .bind(fingerprint)
            .bind(account_id)
            .bind(now_ms)
            .bind(now_ms)
            .execute(&mut *transaction)
            .await?;
        }
    }
    transaction.commit().await
}

fn sqlx_error(error: sqlx::Error) -> ProtocolError {
    ProtocolError::new("auth.sqlite", error.to_string())
}
