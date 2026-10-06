//! The device surface: the paired-device inventory, and a browser's
//! revocation, rotation and logout. The coordinator's own identity is
//! `rpc_identity`; its row in [`METHOD_HANDLERS`] points there.
//!
//! Ported from `apps/coord/src/auth/handlers-devices.ts`. Every mutation ends
//! in a revocation, and every one of them invalidates the JWT key cache AFTER
//! the commit: a cache that learned about a revoked key before the database did
//! would keep resolving a principal the database no longer has.
//!
//! THE IDENTITY COLUMNS ARE PROVENANCE, NOT AUTHORITY. `DevicesList` reports
//! what a device was paired from -- IP, country, user agent, and the Cloudflare
//! Access email when there was one -- read straight out of `authorized_keys`.
//! Nothing in this file promotes a provenance value into a decision.

use std::collections::HashSet;

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;

use crate::auth::authorized_keys::fingerprint_of_raw_public_key;
use crate::auth::bootstrap_tokens::decode_ed25519_pubkey;
use crate::auth::db_statements::{
    AUTHORIZED_KEY, REVOKED_KEY, WORKER_ROW, begin, column1, column2, commit, exists1, exists2,
    insert_account_device, insert_authorized_key, internal, invalid_argument,
};
use crate::auth::jwt_key_cache::JwtKeyCache;
use crate::auth::key_retirement::retire_principal;
use crate::auth::key_revocation::release_revoked_key;
use crate::auth::principal::Principal;
use crate::auth::principal::{device_refusal, require_account_device};
use crate::coord_core::{Caller, CoordCore};
use crate::rpc::service::{now_ms, ok_response};

/// The Connect method each handler answers, and the function that answers it.
///
/// The lead's list: every row is one arm of the single `impl
/// CoordinatorService` block in `rpc/service_impl.rs`, so wiring this domain is
/// reading a table rather than matching on names by hand.
pub const METHOD_HANDLERS: &[(&str, &str)] = &[
    (
        "AuthCoordIdentity",
        "auth::rpc_identity::handle_auth_coord_identity",
    ),
    ("AuthLogout", "auth::rpc_devices::handle_auth_logout"),
    ("DevicesList", "auth::rpc_devices::handle_devices_list"),
    ("DevicesRevoke", "auth::rpc_devices::handle_devices_revoke"),
    (
        "DevicesRotateCurrent",
        "auth::rpc_devices::handle_devices_rotate_current",
    ),
];

/// `CoordinatorService.DevicesList` -- the paired browsers, newest first.
///
/// Workers are filtered OUT: a machine is in `WorkersList`, and a fleet that
/// lists one under "devices" teaches an operator to revoke it here -- which
/// `DevicesRevoke` refuses, pointing at `WorkersDelete` instead.
pub async fn handle_devices_list(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::DevicesListRequest,
) -> ServiceResult<proto::DevicesListResponse> {
    let self_fingerprint = require_account_device(caller)?.to_owned();
    let rows = sqlx::query_as::<_, DeviceColumns>(
        "SELECT fingerprint, label, added_at, paired_from_ip, paired_country, \
         paired_user_agent, paired_edge_identity FROM authorized_keys ORDER BY added_at DESC",
    )
    .fetch_all(core.services.db.pool())
    .await
    .map_err(|error| internal("devices list", &error))?;
    let workers = sqlx::query_as::<_, (String,)>("SELECT fp FROM workers")
        .fetch_all(core.services.db.pool())
        .await
        .map_err(|error| internal("devices list", &error))?;
    let worker_fps: HashSet<&str> = workers.iter().map(|(fp,)| fp.as_str()).collect();

    let devices = rows
        .into_iter()
        .filter(|row| !worker_fps.contains(row.0.as_str()))
        .map(|row| proto::DeviceRow {
            fingerprint: row.0.clone(),
            label: row.1,
            added_at_ms: u64::try_from(row.2).unwrap_or(0),
            is_self: row.0 == self_fingerprint,
            paired_from_ip: row.3.unwrap_or_default(),
            paired_country: row.4.unwrap_or_default(),
            paired_user_agent: row.5.unwrap_or_default(),
            paired_edge_identity: row.6.unwrap_or_default(),
            ..Default::default()
        })
        .collect();
    ok_response(proto::DevicesListResponse {
        devices,
        ..Default::default()
    })
}

/// One `authorized_keys` row, as `DevicesList` reads it.
type DeviceColumns = (
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// `CoordinatorService.DevicesRevoke` -- one paired browser loses its key.
///
/// v2 lets an on-host caller with NO credential run this
/// (`handlers-devices.ts:71-72`), because the operator who lost their only
/// device is exactly who needs it. The gate admits only credentialed browsers
/// today, so that arm is unreachable; the rule is kept so the handler is right
/// the moment the gate widens.
pub async fn handle_devices_revoke(
    core: &CoordCore,
    caller: &Caller,
    request: proto::DevicesRevokeRequest,
) -> ServiceResult<proto::DevicesRevokeResponse> {
    if !caller.principal.is_browser() && !caller.on_host {
        return Err(device_refusal());
    }
    if caller.principal.is_browser() && caller.fingerprint() == request.fingerprint {
        return Err(invalid_argument("use key rotation to revoke this device"));
    }
    let revoked_by = if caller.principal.is_browser() {
        caller.fingerprint().to_owned()
    } else {
        "on-host-recovery".to_owned()
    };
    let fingerprint = request.fingerprint;
    let mut transaction = begin(&core.services.db).await?;
    if !exists1(&mut transaction, AUTHORIZED_KEY, &fingerprint).await? {
        return Err(ConnectError::new(ErrorCode::NotFound, "device not found"));
    }
    if exists1(&mut transaction, WORKER_ROW, &fingerprint).await? {
        return Err(invalid_argument(
            "workers must be deleted through WorkersDelete",
        ));
    }
    retire_principal(
        &mut transaction,
        &fingerprint,
        &revoked_by,
        "device-revoked",
        true,
        None,
        now_ms(),
    )
    .await?;
    commit(transaction).await?;

    core.services.jwt_keys.invalidate_jwt_key(&fingerprint);
    release_revoked_key(&core.services, &fingerprint);
    tracing::info!(%fingerprint, %revoked_by, "device.revoked");
    ok_response(proto::DevicesRevokeResponse {
        ok: true,
        ..Default::default()
    })
}

/// `CoordinatorService.DevicesRotateCurrent` -- swap this browser's key.
///
/// The new key is created and the old one retired in ONE transaction, so a
/// crash between them cannot leave a browser holding a key whose private half it
/// has already forgotten. Rotation also REFRESHES the new key in the cache,
/// unlike every other mutation here: the fingerprint being added is one a
/// verifier may already hold a stale generation for.
pub async fn handle_devices_rotate_current(
    core: &CoordCore,
    caller: &Caller,
    request: proto::DevicesRotateCurrentRequest,
) -> ServiceResult<proto::DevicesRotateCurrentResponse> {
    let current = require_account_device(caller)?.to_owned();
    let public_key = decode_ed25519_pubkey(&request.ssh_pubkey_b64)
        .ok_or_else(|| invalid_argument("invalid ssh_pubkey_b64"))?;
    let fingerprint = fingerprint_of_raw_public_key(&public_key);
    if fingerprint == current {
        return Err(invalid_argument("new key matches current key"));
    }
    let now = now_ms();
    let mut transaction = begin(&core.services.db).await?;
    if exists1(&mut transaction, REVOKED_KEY, &fingerprint).await? {
        return Err(ConnectError::new(
            ErrorCode::PermissionDenied,
            "new key was previously revoked",
        ));
    }
    if exists1(&mut transaction, AUTHORIZED_KEY, &fingerprint).await?
        || exists1(&mut transaction, WORKER_ROW, &fingerprint).await?
    {
        return Err(ConnectError::new(
            ErrorCode::AlreadyExists,
            "new key is already in use",
        ));
    }
    // The account is re-read rather than taken from the principal: a legacy
    // self-hosted key has none, and an account device whose row has gone must
    // not silently rejoin an account it no longer belongs to.
    let account_id: Option<String> =
        match &caller.principal {
            Principal::AccountDevice { account_id, .. } => column2(
                &mut transaction,
                "SELECT account_id FROM account_devices WHERE fingerprint = $1 AND account_id = $2",
                (current.as_str(), account_id.as_str()),
            )
            .await?,
            _ => {
                column1(
                    &mut transaction,
                    "SELECT account_id FROM account_devices WHERE fingerprint = $1",
                    current.as_str(),
                )
                .await?
            }
        };
    insert_authorized_key(
        &mut transaction,
        &fingerprint,
        &public_key,
        &request.label,
        now,
    )
    .await?;
    if let Some(account_id) = account_id {
        insert_account_device(&mut transaction, &fingerprint, &account_id, now).await?;
    }
    retire_principal(
        &mut transaction,
        &current,
        &current,
        "device-rotated",
        true,
        None,
        now,
    )
    .await?;
    commit(transaction).await?;

    rotate_key_cache(&core.services.jwt_keys, &fingerprint, &current);
    release_revoked_key(&core.services, &current);
    tracing::info!(previous = %current, %fingerprint, "device.rotated");
    ok_response(proto::DevicesRotateCurrentResponse {
        fingerprint,
        ..Default::default()
    })
}

/// `CoordinatorService.AuthLogout` -- this browser throws its own key away.
///
/// A logout is a self-revocation, and it refuses when the key is not there: a
/// logout that reported success for a fingerprint the database does not hold
/// would tell a browser it is signed out while it still holds a live key.
pub async fn handle_auth_logout(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::AuthLogoutRequest,
) -> ServiceResult<proto::AuthLogoutResponse> {
    let fingerprint = require_account_device(caller)?.to_owned();
    let account_id = match &caller.principal {
        Principal::AccountDevice { account_id, .. } => Some(account_id.clone()),
        _ => None,
    };
    let mut transaction = begin(&core.services.db).await?;
    let owned = match &account_id {
        Some(account_id) => {
            exists2(
                &mut transaction,
                "SELECT 1 FROM authorized_keys AS key JOIN account_devices AS device \
                 ON device.fingerprint = key.fingerprint \
                 WHERE key.fingerprint = $1 AND device.account_id = $2",
                (fingerprint.as_str(), account_id.as_str()),
            )
            .await?
        }
        None => exists1(&mut transaction, AUTHORIZED_KEY, &fingerprint).await?,
    };
    if !owned {
        return Err(ConnectError::new(
            ErrorCode::Unauthenticated,
            "authentication required",
        ));
    }
    retire_principal(
        &mut transaction,
        &fingerprint,
        &fingerprint,
        "browser-logout",
        false,
        account_id.as_deref(),
        now_ms(),
    )
    .await?;
    commit(transaction).await?;

    core.services.jwt_keys.invalidate_jwt_key(&fingerprint);
    release_revoked_key(&core.services, &fingerprint);
    tracing::info!(%fingerprint, "device.logged_out");
    ok_response(proto::AuthLogoutResponse {
        ok: true,
        ..Default::default()
    })
}

/// Rotation is the one mutation that both adds and removes a key, so its cache
/// effects are the one pair that is not "invalidate what we touched".
fn rotate_key_cache(cache: &JwtKeyCache, added: &str, removed: &str) {
    cache.refresh_jwt_key(added);
    cache.invalidate_jwt_key(removed);
}
