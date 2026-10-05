//! `CoordinatorService.AuthCoordIdentity` — what a client learns about a
//! coordinator before it has any credential at all.
//!
//! Ported from `handlers-auth-bootstrap.ts:54-60`. It is the ONE RPC a pairing
//! ceremony cannot proceed without and the one `roost quickstart` blocks on, so
//! its shape is a product decision rather than an implementation detail: a
//! browser reads `public_url` to learn which origin its keys belong to, and an
//! installer reads `git_sha` to learn which build it just reached.
//!
//! IT IS PUBLIC, AND IT ANSWERS ALMOST NOTHING. No caller is resolved, because
//! resolving one is what would make it non-public, and nothing here is
//! per-caller: the fields are this process's build, this deployment's own
//! declared origin, and the direct carrier's static STUN settings (hostnames,
//! not secrets), which let a browser start gathering before its first grant.
//! The relocation, SaaS-mode and listener fields stay at their defaults, which
//! is what v2 sends — they describe a managed deployment and a self-hosted
//! install has none of them to report.
//!
//! `public_url` FALLS BACK TO `web_public_url` AND THEN TO THE EMPTY STRING,
//! because a client that asked and got a field it can read is better served
//! than one that got an error: an install that declared only a front door still
//! answers, and the empty string is a truth about the deployment rather than a
//! failure of the query.

use connectrpc::ServiceResult;
use roost_host::CoordConfig;
use roost_proto as proto;

use crate::rpc::service::ok_response;

/// `CoordinatorService.AuthCoordIdentity`.
pub fn handle_auth_coord_identity(
    config: &CoordConfig,
    git_sha: &str,
) -> ServiceResult<proto::AuthCoordIdentityResponse> {
    ok_response(proto::AuthCoordIdentityResponse {
        git_sha: git_sha.to_owned(),
        public_url: config
            .public_url
            .clone()
            .or_else(|| config.web_public_url.clone())
            .unwrap_or_default(),
        terminal_peer_enabled: config.terminal_peer_enabled,
        terminal_peer_stun_urls: if config.terminal_peer_enabled {
            config.terminal_peer_stun_urls.clone()
        } else {
            Vec::new()
        },
        // A self-hosted install was never relocated, is not a managed
        // deployment, and is not a public listener. Left absent rather than
        // filled in, which is what v2 sends and what a client reads as "this
        // deployment has nothing to report".
        ..Default::default()
    })
}
