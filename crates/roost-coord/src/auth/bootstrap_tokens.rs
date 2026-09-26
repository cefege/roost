//! Ported from `apps/coord/src/auth/bootstrap-tokens.ts`. A bootstrap token is
//! how a fresh machine or a fresh browser joins a fleet without a pairing
//! ceremony, which is exactly why it is scoped, time-bounded and redeemable
//! ONCE: a token that could be redeemed twice would be a permanent credential
//! for anyone who ever saw it. The plaintext bearer exists only in the minting
//! call's return value; SQLite stores a scoped SHA-256 digest and the claim
//! state, so a stolen database file yields no working token.
//!
//! WHY THE STATEMENT PLUMBING IS HERE. `rpc_devices` retires the same key rows
//! this file claims against, and both need to open a transaction and run a typed
//! statement. Keeping those helpers here rather than in an RPC module keeps the
//! dependency pointing DOWN -- the device and bootstrap RPCs ask this file, never
//! the reverse -- so a change to either surface cannot break the other for no
//! visible reason.
//!
//! THE CLAIM IS ONE `UPDATE`, NOT A READ THEN A WRITE. [`claim_bootstrap_token`]
//! decides single-use inside the same statement that records the claim, so two
//! simultaneous redemptions of one token cannot both observe "unused". A
//! read-then-write would depend on the caller's transaction being serializable,
//! and SQLite's is not: both readers would see `used_at_ms IS NULL`.

use std::io::Read as _;

use base64::Engine as _;
use base64::engine::general_purpose;
use roost_protocol::{ProtocolError, ProtocolResult};
use sha2::Digest as _;
use sqlx::{Sqlite, Transaction};

use crate::db::CoordDb;

/// The prefix that makes a token recognisable in a log a human reads.
pub const BOOTSTRAP_TOKEN_PREFIX: &str = "roost_bt_";

/// How long a minted token stays redeemable: 24 h (`bootstrap-tokens.ts:45`).
pub const BOOTSTRAP_TOKEN_TTL_MS: i64 = 24 * 60 * 60 * 1_000;

/// The randomness in a token: 24 bytes, rendered as 48 hex characters
/// (`bootstrap-tokens.ts:46`).
pub const BOOTSTRAP_TOKEN_RANDOM_BYTES: usize = 24;

/// The width of a raw Ed25519 public key.
const PUBLIC_KEY_BYTES: usize = 32;

/// The OpenSSH key-type name a wire-format public key is wrapped in.
const SSH_ED25519_TYPE: &[u8] = b"ssh-ed25519";

/// Which kind of principal a grant may create.
///
/// The kind is part of the claim predicate, not decoration: a worker grant may
/// not create a browser and a browser grant may not create a machine, because
/// the two answer to different rules everywhere else -- a worker may not call an
/// operator method, and a browser may not open a worker socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapTokenKind {
    /// Redeems into a `workers` row.
    Worker,
    /// Redeems into an `account_devices` row.
    Browser,
}

impl BootstrapTokenKind {
    /// The stored spelling, which the schema's `CHECK` constrains.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::Browser => "browser",
        }
    }

    /// Parse the wire spelling, or `None` for anything else. An unknown kind is
    /// a refusal rather than a default: "browser " with a trailing space must
    /// not become a worker.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "worker" => Some(Self::Worker),
            "browser" => Some(Self::Browser),
            _ => None,
        }
    }
}

/// A freshly minted grant. The only value anywhere that holds the live bearer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedBootstrapToken {
    /// The plaintext bearer, returned once and never stored.
    pub token: String,
    /// When the grant stops being redeemable, in epoch milliseconds.
    pub expires_at_ms: u64,
}

/// What a claimed grant entitles its bearer to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapTokenClaim {
    /// The account the new principal belongs to.
    pub account_id: String,
    /// The label the minter chose, for the operator's own list.
    pub label: String,
    /// The device that minted it, or `None` for a host-local quickstart grant.
    pub minted_by_fp: Option<String>,
}

/// The scoped digest SQLite stores instead of the bearer.
#[must_use]
pub fn bootstrap_token_digest(plaintext: &str) -> String {
    let digest: [u8; 32] = sha2::Sha256::digest(plaintext.as_bytes()).into();
    roost_protocol::fingerprint::fingerprint_hex(&digest)
}

/// Decode the public key a redeeming device or worker presented.
///
/// Accepts a raw 32-byte key in standard or URL-safe base64 (what a browser and
/// a worker both send) or the OpenSSH wire form
/// `<u32 len><"ssh-ed25519"><u32 32><32 bytes>` a file import carries. The wire
/// form is checked field by field rather than sliced at a fixed offset, because
/// a slice accepts any document long enough (`device-revocation.test.ts`,
/// "rotation rejects malformed SSH wire keys instead of slicing arbitrary
/// bytes").
#[must_use]
pub fn decode_ed25519_pubkey(encoded: &str) -> Option<[u8; PUBLIC_KEY_BYTES]> {
    // One decoder, deliberately lenient about alphabet and padding: a browser
    // sends URL-safe, a file import sends standard, and refusing either fails an
    // enrollment that has a perfectly good key.
    // `GeneralPurpose::new` takes the raw `Alphabet`, not a `GeneralPurpose`:
    // the shipped `STANDARD` engine is a `GeneralPurpose`, so passing it back
    // into the constructor is a type error rather than a reuse. The shipped
    // `STANDARD_*` engines also all refuse trailing bits, which `Buffer.from(x,
    // "base64")` accepts, so the engine has to be built here to stay lenient in
    // both directions.
    const DECODER: general_purpose::GeneralPurpose = general_purpose::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        general_purpose::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    let raw = DECODER
        .decode(encoded.replace('-', "+").replace('_', "/"))
        .ok()?;
    if raw.len() == PUBLIC_KEY_BYTES {
        return <[u8; PUBLIC_KEY_BYTES]>::try_from(raw).ok();
    }
    ssh_wire_key(&raw)
}

/// The key bytes out of an OpenSSH wire document, or `None` if it is not one.
fn ssh_wire_key(raw: &[u8]) -> Option<[u8; PUBLIC_KEY_BYTES]> {
    let type_end = 4usize.checked_add(SSH_ED25519_TYPE.len())?;
    if raw.len() < type_end + 4 + PUBLIC_KEY_BYTES
        || u32::from_be_bytes(raw[..4].try_into().ok()?) as usize != SSH_ED25519_TYPE.len()
        || &raw[4..type_end] != SSH_ED25519_TYPE
        || u32::from_be_bytes(raw[type_end..type_end + 4].try_into().ok()?) as usize
            != PUBLIC_KEY_BYTES
    {
        return None;
    }
    <[u8; PUBLIC_KEY_BYTES]>::try_from(&raw[type_end + 4..type_end + 4 + PUBLIC_KEY_BYTES]).ok()
}

/// A fresh bearer, from the operating system's CSPRNG.
///
/// `/dev/urandom` rather than a `rand` dependency: it is the generator
/// `crypto.getRandomValues` reads and it is present on both platforms this
/// project ships. A read failure is an error, never a fallback -- a token whose
/// entropy came from anything weaker is a permanent credential for whoever
/// guessed it.
fn random_bootstrap_bearer() -> ProtocolResult<String> {
    let mut random = [0_u8; BOOTSTRAP_TOKEN_RANDOM_BYTES];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut random))
        .map_err(|error| {
            ProtocolError::new(
                "auth.bootstrap_tokens",
                format!("no secure randomness for a bootstrap bearer: {error}"),
            )
        })?;
    let mut token = String::with_capacity(BOOTSTRAP_TOKEN_PREFIX.len() + random.len() * 2);
    token.push_str(BOOTSTRAP_TOKEN_PREFIX);
    token.push_str(&hex::encode(random));
    Ok(token)
}

/// Mint a one-shot grant on an authenticated device's behalf.
///
/// `account_id` and `dashboard_id` are the tenancy scope the grant is confined
/// to, and `minted_by_fp` is what lets a revocation take the grant with it: an
/// unused grant is deleted when its minter is revoked, so a stolen token cannot
/// outlive the device that was trusted to issue it.
pub async fn mint_bootstrap_token(
    database: &CoordDb,
    kind: BootstrapTokenKind,
    label: &str,
    account_id: &str,
    dashboard_id: &str,
    minted_by_fp: Option<&str>,
    now_ms: i64,
) -> ProtocolResult<MintedBootstrapToken> {
    let token = random_bootstrap_bearer()?;
    let token_hash = bootstrap_token_digest(&token);
    let expires_at_ms = now_ms + BOOTSTRAP_TOKEN_TTL_MS;
    sqlx::query(
        "INSERT INTO bootstrap_tokens (token_hash, account_id, dashboard_id, kind, label, \
         created_at_ms, expires_at_ms, used_at_ms, used_by_fp, minted_by_fp) \
         VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?)",
    )
    .bind(token_hash.as_str())
    .bind(account_id)
    .bind(dashboard_id)
    .bind(kind.as_str())
    .bind(label)
    .bind(now_ms)
    .bind(expires_at_ms)
    .bind(minted_by_fp)
    .execute(database.pool())
    .await
    .map_err(|error| refuse(format!("insert bootstrap token: {error}")))?;

    tracing::info!(kind = kind.as_str(), "auth.bootstrap_minted");
    Ok(MintedBootstrapToken {
        token,
        expires_at_ms: u64::try_from(expires_at_ms).unwrap_or(0),
    })
}

/// Mint a grant with no minter, for `roost quickstart` on a fresh host.
///
/// A minter-less grant is redeemable by whoever holds it until it expires, which
/// is the point: it is how a machine joins before any device exists to mint one.
/// It is also why revoking any device DELETES every unused minter-less grant.
pub async fn mint_host_bootstrap_token(
    database: &CoordDb,
    kind: BootstrapTokenKind,
    label: &str,
    now_ms: i64,
) -> ProtocolResult<MintedBootstrapToken> {
    let tenant =
        crate::auth::self_hosted_tenant::ensure_self_hosted_tenant(database, now_ms).await?;
    mint_bootstrap_token(
        database,
        kind,
        label,
        &tenant.account_id,
        &tenant.dashboard_id,
        None,
        now_ms,
    )
    .await
}

/// What a redemption presents against a grant.
#[derive(Debug)]
pub struct BootstrapClaim<'a> {
    /// The digest of the bearer the caller presented.
    pub token_hash: &'a str,
    /// Which principal kind the caller is trying to create.
    pub kind: BootstrapTokenKind,
    /// The fingerprint the presented public key resolves to.
    pub fingerprint: &'a str,
    /// The raw public key, checked byte-for-byte on a retry.
    pub public_key: &'a [u8; PUBLIC_KEY_BYTES],
    /// The instant the claim is evaluated at.
    pub now_ms: i64,
}

/// Claim an unused grant, or recognise an exact same-key retry of one that
/// already created its principal.
///
/// The caller MUST run this in the same transaction that creates or validates
/// the principal: the claim and the principal are one fact, and a claim that
/// commits separately can spend a grant whose principal was never created.
///
/// Every condition in the `WHERE` clause is a refusal, and together they are the
/// grant's whole security surface: the digest and kind must match, it must not
/// have expired, the redeeming fingerprint must not itself be revoked, the
/// account must still be active, and either nobody minted it or the minter is
/// still an unrevoked device of the same account.
///
/// The retry arm exists because a lost response is indistinguishable from a
/// failed one. It requires the SAME fingerprint, the SAME public key and a
/// principal already consistent with the grant, so it cannot enroll a second
/// machine with one token.
pub async fn claim_bootstrap_token(
    transaction: &mut Transaction<'_, Sqlite>,
    claim: &BootstrapClaim<'_>,
) -> ProtocolResult<Option<BootstrapTokenClaim>> {
    let row = sqlx::query_as::<_, (String, String, Option<String>)>(
        "UPDATE bootstrap_tokens AS bt \
         SET used_at_ms = CASE WHEN bt.used_at_ms IS NULL THEN ? ELSE bt.used_at_ms END, \
             used_by_fp = CASE WHEN bt.used_at_ms IS NULL THEN ? ELSE bt.used_by_fp END \
         WHERE bt.token_hash = ? AND bt.kind = ? AND bt.expires_at_ms >= ? \
           AND NOT EXISTS (SELECT 1 FROM authorized_key_revocations AS submitted \
                           WHERE submitted.fingerprint = ?) \
           AND EXISTS (SELECT 1 FROM accounts AS account \
                       WHERE account.id = bt.account_id AND account.status = 'active') \
           AND (bt.minted_by_fp IS NULL OR EXISTS ( \
                 SELECT 1 FROM authorized_keys AS minter_key \
                 JOIN account_devices AS minter_device \
                   ON minter_device.fingerprint = minter_key.fingerprint \
                  AND minter_device.account_id = bt.account_id \
                 WHERE minter_key.fingerprint = bt.minted_by_fp AND NOT EXISTS ( \
                   SELECT 1 FROM authorized_key_revocations AS minter_revocation \
                   WHERE minter_revocation.fingerprint = minter_key.fingerprint))) \
           AND ((bt.used_at_ms IS NULL \
                 AND NOT EXISTS (SELECT 1 FROM authorized_keys AS fresh_key \
                                 WHERE fresh_key.fingerprint = ?) \
                 AND NOT EXISTS (SELECT 1 FROM workers AS fresh_worker \
                                 WHERE fresh_worker.fp = ?) \
                 AND NOT EXISTS (SELECT 1 FROM account_devices AS fresh_device \
                                 WHERE fresh_device.fingerprint = ?)) \
             OR (bt.used_at_ms IS NOT NULL AND bt.used_by_fp = ? \
                 AND EXISTS (SELECT 1 FROM authorized_keys AS retry_key \
                             WHERE retry_key.fingerprint = ? AND retry_key.public_key = ?) \
                 AND ((? = 'worker' AND EXISTS (SELECT 1 FROM workers AS retry_worker \
                             WHERE retry_worker.fp = ? AND retry_worker.deleted_at_ms IS NULL)) \
                   OR (? = 'browser' AND EXISTS (SELECT 1 FROM account_devices AS retry_device \
                             WHERE retry_device.fingerprint = ? \
                               AND retry_device.account_id = bt.account_id)))) \
         RETURNING bt.account_id, bt.label, bt.minted_by_fp",
    )
    .bind(claim.now_ms)
    .bind(claim.fingerprint)
    .bind(claim.token_hash)
    .bind(claim.kind.as_str())
    .bind(claim.now_ms)
    .bind(claim.fingerprint)
    .bind(claim.fingerprint)
    .bind(claim.fingerprint)
    .bind(claim.fingerprint)
    .bind(claim.fingerprint)
    .bind(claim.fingerprint)
    .bind(claim.public_key.as_slice())
    .bind(claim.kind.as_str())
    .bind(claim.fingerprint)
    .bind(claim.kind.as_str())
    .bind(claim.fingerprint)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| refuse(format!("claim bootstrap token: {error}")))?;

    Ok(
        row.map(|(account_id, label, minted_by_fp)| BootstrapTokenClaim {
            account_id,
            label,
            minted_by_fp,
        }),
    )
}

/// One wording for every refusal in this module. A redeemed-but-refused token
/// is `None`, and the handler turns that into a single `Unauthenticated`
/// regardless of which predicate fired: telling a caller WHICH condition its
/// token failed turns a probe into an oracle for the account's state.
fn refuse(reason: String) -> ProtocolError {
    ProtocolError::new("auth.bootstrap_tokens", reason)
}
