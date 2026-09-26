//! The bootstrap surface: minting a one-shot enrollment grant, and the worker
//! and browser redemptions that spend one. Ported from
//! `apps/coord/src/auth/handlers-auth-bootstrap.ts`.
//!
//! The claim and the principal it creates are ONE transaction in both
//! redemptions: a grant spent without a principal behind it is a grant nobody
//! can use again, and a lost response is indistinguishable from a failure. The
//! claim, the key rows and the statements that write them all live below this

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_platform::HostPlatform;
use roost_proto as proto;
use sqlx::{Sqlite, Transaction};

use crate::auth::authorized_keys::fingerprint_of_raw_public_key;
use crate::auth::bootstrap_tokens::{
    self, BootstrapClaim, BootstrapTokenClaim, BootstrapTokenKind, claim_bootstrap_token,
    decode_ed25519_pubkey, mint_bootstrap_token,
};
use crate::auth::db_statements::{
    ACCOUNT_DEVICE_ROW, Bind, WORKER_ROW, begin, column1, commit, exists1, fault,
    insert_account_device, insert_authorized_key, invalid_argument, require_account_device, run,
    stored_public_key,
};
use crate::coord_core::{Caller, CoordCore};
use crate::db::CoordDb;
use crate::events::persistence_input::{MAX_PERSISTED_UTF8_BYTES, truncate_persisted_utf8};
use crate::rpc::service::{now_ms, ok_response};

/// The Connect method each handler answers, and the function that answers it.
pub const METHOD_HANDLERS: &[(&str, &str)] = &[
    (
        "AuthMintBootstrap",
        "auth::rpc_bootstrap::handle_auth_mint_bootstrap",
    ),
    (
        "AuthRedeemWorker",
        "auth::rpc_bootstrap::handle_auth_redeem_worker",
    ),
    (
        "AuthRedeemBrowser",
        "auth::rpc_bootstrap::handle_auth_redeem_browser",
    ),
];

/// `CoordinatorService.AuthMintBootstrap` -- one one-shot enrollment grant.
pub async fn handle_auth_mint_bootstrap(
    core: &CoordCore,
    caller: &Caller,
    request: proto::AuthMintBootstrapRequest,
) -> ServiceResult<proto::AuthMintBootstrapResponse> {
    let minter = require_account_device(caller)?.to_owned();
    let kind = BootstrapTokenKind::parse(&request.kind)
        .ok_or_else(|| invalid_argument("bootstrap kind must be worker or browser"))?;
    let tenant = core.services.boot.require_tenant()?;
    let label = truncate_persisted_utf8(&request.label, MAX_PERSISTED_UTF8_BYTES);
    let minted = mint_bootstrap_token(
        &core.services.db,
        kind,
        label,
        &tenant.account_id,
        &tenant.dashboard_id,
        Some(&minter),
        now_ms(),
    )
    .await
    .map_err(|error| fault("redeem", &error))?;

    ok_response(proto::AuthMintBootstrapResponse {
        token: minted.token,
        expires_at_ms: minted.expires_at_ms,
        ..Default::default()
    })
}

/// `CoordinatorService.AuthRedeemWorker` -- a machine joins the fleet.
pub async fn handle_auth_redeem_worker(
    core: &CoordCore,
    _caller: &Caller,
    request: proto::AuthRedeemWorkerRequest,
) -> ServiceResult<proto::AuthRedeemWorkerResponse> {
    if !HostPlatform::is_supported(&request.os) {
        return Err(invalid_argument("unsupported worker os"));
    }
    let public_key = decode_ed25519_pubkey(&request.ssh_pubkey_b64).ok_or_else(invalid_pubkey)?;
    let fingerprint = fingerprint_of_raw_public_key(&public_key);
    let now = now_ms();
    let label = truncate_persisted_utf8(&request.label, MAX_PERSISTED_UTF8_BYTES).to_owned();
    let git_sha = request
        .git_sha
        .as_ref()
        .map(|sha| truncate_persisted_utf8(sha, MAX_PERSISTED_UTF8_BYTES).to_owned());

    let dashboard_id = core.services.boot.require_tenant()?.dashboard_id.clone();
    let (mut transaction, _) = claim_redemption(
        &core.services.db,
        &request.token,
        BootstrapTokenKind::Worker,
        &fingerprint,
        &public_key,
        now,
    )
    .await?;

    // A fingerprint that is already a browser's cannot also become a machine:
    // `resolve_principal` refuses a dual-authority key, so one created here
    // would be a device that can never authenticate.
    if exists1(&mut transaction, ACCOUNT_DEVICE_ROW, &fingerprint).await? {
        return Err(invalid_bootstrap_token());
    }
    let stored = stored_public_key(&mut transaction, &fingerprint).await?;
    if exists1(&mut transaction, WORKER_ROW, &fingerprint).await? {
        if stored.as_deref() != Some(public_key.as_slice()) {
            return Err(invalid_bootstrap_token());
        }
        run(
            &mut transaction,
            "UPDATE authorized_keys SET label = ? WHERE fingerprint = ?",
            &[Bind::Text(Some(&label)), Bind::Text(Some(&fingerprint))],
        )
        .await?;
        run(
            &mut transaction,
            "UPDATE workers SET label = ?, os = ?, git_sha = ?, last_seen_ms = ? WHERE fp = ?",
            &[
                Bind::Text(Some(&label)),
                Bind::Text(Some(&request.os)),
                Bind::Text(git_sha.as_deref()),
                Bind::Int(now),
                Bind::Text(Some(&fingerprint)),
            ],
        )
        .await?;
    } else {
        if stored.is_some() {
            return Err(invalid_bootstrap_token());
        }
        insert_authorized_key(&mut transaction, &fingerprint, &public_key, &label, now).await?;
        run(
            &mut transaction,
            "INSERT INTO workers (fp, dashboard_id, label, os, git_sha, host_metrics_json, \
             registered_at_ms, last_seen_ms) VALUES (?, ?, ?, ?, ?, NULL, ?, ?)",
            &[
                Bind::Text(Some(&fingerprint)),
                Bind::Text(Some(&dashboard_id)),
                Bind::Text(Some(&label)),
                Bind::Text(Some(&request.os)),
                Bind::Text(git_sha.as_deref()),
                Bind::Int(now),
                Bind::Int(now),
            ],
        )
        .await?;
    }
    commit(transaction).await?;

    core.services.jwt_keys.refresh_jwt_key(&fingerprint);
    tracing::info!(%fingerprint, kind = "worker", "auth.worker_redeemed");
    ok_response(proto::AuthRedeemWorkerResponse {
        fingerprint,
        label,
        ..Default::default()
    })
}

/// `CoordinatorService.AuthRedeemBrowser` -- a browser joins the fleet.
pub async fn handle_auth_redeem_browser(
    core: &CoordCore,
    _caller: &Caller,
    request: proto::AuthRedeemBrowserRequest,
) -> ServiceResult<proto::AuthRedeemBrowserResponse> {
    let public_key = decode_ed25519_pubkey(&request.ssh_pubkey_b64).ok_or_else(invalid_pubkey)?;
    let fingerprint = fingerprint_of_raw_public_key(&public_key);
    let now = now_ms();
    let label = truncate_persisted_utf8(&request.label, MAX_PERSISTED_UTF8_BYTES).to_owned();
    let (mut transaction, claimed) = claim_redemption(
        &core.services.db,
        &request.token,
        BootstrapTokenKind::Browser,
        &fingerprint,
        &public_key,
        now,
    )
    .await?;

    if exists1(&mut transaction, WORKER_ROW, &fingerprint).await? {
        return Err(invalid_bootstrap_token());
    }
    let stored = stored_public_key(&mut transaction, &fingerprint).await?;
    let device_account = column1(
        &mut transaction,
        "SELECT account_id FROM account_devices WHERE fingerprint = ?",
        &fingerprint,
    )
    .await?;
    if stored.is_some() || device_account.is_some() {
        // A retry is only a retry when the principal it finds is the one this
        // grant would have created: same key bytes, same account.
        if stored.as_deref() != Some(public_key.as_slice())
            || device_account.as_deref() != Some(claimed.account_id.as_str())
        {
            return Err(invalid_bootstrap_token());
        }
        run(
            &mut transaction,
            "UPDATE authorized_keys SET label = ? WHERE fingerprint = ?",
            &[Bind::Text(Some(&label)), Bind::Text(Some(&fingerprint))],
        )
        .await?;
        run(
            &mut transaction,
            "UPDATE account_devices SET last_seen_at_ms = ? WHERE fingerprint = ? \
             AND account_id = ?",
            &[
                Bind::Int(now),
                Bind::Text(Some(&fingerprint)),
                Bind::Text(Some(&claimed.account_id)),
            ],
        )
        .await?;
    } else {
        insert_authorized_key(&mut transaction, &fingerprint, &public_key, &label, now).await?;
        insert_account_device(&mut transaction, &fingerprint, &claimed.account_id, now).await?;
    }
    commit(transaction).await?;

    core.services.jwt_keys.refresh_jwt_key(&fingerprint);
    tracing::info!(%fingerprint, kind = "browser", "auth.browser_redeemed");
    ok_response(proto::AuthRedeemBrowserResponse::default())
}

/// would burn a grant for nothing.
async fn claim_redemption<'a>(
    database: &'a CoordDb,
    token: &str,
    kind: BootstrapTokenKind,
    fingerprint: &str,
    public_key: &[u8; 32],
    now: i64,
) -> Result<(Transaction<'a, Sqlite>, BootstrapTokenClaim), ConnectError> {
    let mut transaction = begin(database).await?;
    let token_hash = bootstrap_tokens::bootstrap_token_digest(token);
    let claim = BootstrapClaim {
        token_hash: &token_hash,
        kind,
        fingerprint,
        public_key,
        now_ms: now,
    };
    let claimed = claim_bootstrap_token(&mut transaction, &claim)
        .await
        .map_err(|error| fault("claim bootstrap token", &error))?
        .ok_or_else(invalid_bootstrap_token)?;
    Ok((transaction, claimed))
}

fn invalid_pubkey() -> ConnectError {
    invalid_argument("invalid ssh_pubkey_b64")
}

fn invalid_bootstrap_token() -> ConnectError {
    ConnectError::new(ErrorCode::Unauthenticated, "invalid or expired token")
}

