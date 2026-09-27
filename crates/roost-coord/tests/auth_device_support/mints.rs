//! Minting and redeeming a bootstrap grant, split out of `mod.rs`, which keeps
//! the scratch database, the callers and the key derivation.
//!
//! A GRANT IS A FIXTURE, NOT A STRING: every one of these goes through the real
//! mint path and stamps the coordinator's own clock, because a grant minted with
//! a literal instant is one `claim_bootstrap_token` refuses as already expired.

use connectrpc::ConnectError;

use roost_coord::auth::bootstrap_tokens::BootstrapTokenKind;
use roost_coord::auth::principal::Caller;
use roost_coord::auth::rpc_bootstrap::{
    handle_auth_mint_bootstrap, handle_auth_redeem_browser, handle_auth_redeem_worker,
};
use roost_coord::coord_core::CoordCore;
use roost_coord::db::CoordDb;
use roost_proto as proto;

use super::{anonymous, pubkey_b64};
/// A browser redemption of `token` by `label`'s key.
pub fn redeem_browser(
    token: &str,
    label: &str,
    device_label: &str,
) -> proto::AuthRedeemBrowserRequest {
    proto::AuthRedeemBrowserRequest {
        token: token.to_owned(),
        ssh_pubkey_b64: pubkey_b64(label),
        label: device_label.to_owned(),
        ..Default::default()
    }
}

/// A worker redemption of `token` by `label`'s key.
pub fn redeem_worker(
    token: &str,
    label: &str,
    worker_label: &str,
) -> proto::AuthRedeemWorkerRequest {
    // `..Default::default()` covers `__buffa_unknown_fields`, the sink buffa
    // generates on every message. Naming the generated field instead would be
    // reading a compiler artefact.
    proto::AuthRedeemWorkerRequest {
        token: token.to_owned(),
        ssh_pubkey_b64: pubkey_b64(label),
        label: worker_label.to_owned(),
        os: "linux".to_owned(),
        git_sha: Some("test-sha".to_owned()),
        ..Default::default()
    }
}

/// Mint a grant through the handler, as a paired device would.
pub async fn mint_via_handler(
    core: &CoordCore,
    minter: &Caller,
    kind: &str,
    label: &str,
) -> String {
    handle_auth_mint_bootstrap(
        core,
        minter,
        proto::AuthMintBootstrapRequest {
            kind: kind.to_owned(),
            label: label.to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("a minted grant")
    .body
    .token
}

/// Mint a grant straight against the state layer, for the tests that are about
/// the token rather than about the RPC.
///
/// The instant is the coordinator's OWN clock. This fixture stamps synthetic
/// ones elsewhere (`added_at = 1000`) because nothing compares them, but
/// `claim_bootstrap_token` refuses a grant whose `expires_at_ms` is behind
/// `now_ms` -- a literal here mints one that expired in 1970.
pub async fn mint_grant(
    database: &CoordDb,
    tenancy: (&str, &str),
    kind: BootstrapTokenKind,
    label: &str,
    minter: Option<&str>,
) -> String {
    roost_coord::auth::bootstrap_tokens::mint_bootstrap_token(
        database,
        kind,
        label,
        tenancy.0,
        tenancy.1,
        minter,
        roost_coord::rpc::service::now_ms(),
    )
    .await
    .expect("a grant")
    .token
}

/// Mint a grant nobody is accountable for, the way `roost quickstart` does.
pub async fn mint_host_grant(database: &CoordDb, kind: BootstrapTokenKind, label: &str) -> String {
    roost_coord::auth::bootstrap_tokens::mint_host_bootstrap_token(
        database,
        kind,
        label,
        roost_coord::rpc::service::now_ms(),
    )
    .await
    .expect("a host grant")
    .token
}

/// Redeem a browser grant through the handler.
pub async fn redeem_browser_via_handler(
    core: &CoordCore,
    request: proto::AuthRedeemBrowserRequest,
) -> Result<proto::AuthRedeemBrowserResponse, ConnectError> {
    // A handler answers `ServiceResult<T>`, which is connectrpc's
    // `Result<Response<T>, ConnectError>` -- the envelope, not the message. The
    // fixture promises the message, so the envelope is unwrapped here rather
    // than in every caller.
    handle_auth_redeem_browser(core, &anonymous(), request)
        .await
        .map(|response| response.body)
}

/// Redeem a worker grant through the handler.
pub async fn redeem_worker_via_handler(
    core: &CoordCore,
    request: proto::AuthRedeemWorkerRequest,
) -> Result<proto::AuthRedeemWorkerResponse, ConnectError> {
    handle_auth_redeem_worker(core, &anonymous(), request)
        .await
        .map(|response| response.body)
}
