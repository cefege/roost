//! The four transcription Connect methods: read the settings, write them, hand
//! the stored key to an authenticated browser, and probe the provider.
//!
//! Ported from `apps/coord/src/rpc/handlers-transcription.ts` over
//! `diagnostics/transcription.rs`, which owns the settings rows and the probe.
//!
//! THE HANDOFF IS THE CONFIGURED KEY, NOT A GRANT. Deepgram restricted keys
//! cannot mint `/v1/auth/grant` tokens -- the provider answers 403 -- so
//! `transcriptionGrantToken` returns the stored key with `expires_in` zero
//! rather than a temporary credential, and a browser connects to Deepgram
//! directly (`transcription.ts:75-85`). It is therefore refused to anything
//! that is not an authenticated device: the key is the credential, and a
//! machine caller is not the operator a self-hosted coordinator has.
//!
//! THE PROBE'S OUTCOME IS THE RPC's ANSWER, AND IT IS ALSO KEPT. The caller
//! awaits the provider, so `TranscriptionTest` blocks on a third party's
//! latency; the deadline in `transcription.rs` is what stops that from being
//! unbounded, and the settled state is what makes the attempt readable
//! afterwards rather than only to the request that made it.

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;
use tracing::info;

use crate::coord_core::{Caller, CoordCore};
use crate::auth::principal::require_account_device;
use crate::diagnostics::transcription::{self, ProviderProbe, TranscriptionStoreError};
use crate::rpc::service::ok_response;

/// `CoordinatorService.TranscriptionGetConfig` -- the settings, never the key.
pub async fn handle_transcription_get_config(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::TranscriptionGetConfigRequest,
) -> ServiceResult<proto::TranscriptionConfig> {
    require_account_device(caller)?;
    let scope = tenant_scope(core)?;
    let config = transcription::load_config(core.services.db.pool(), scope)
        .await
        .map_err(refuse_store)?;
    ok_response(config_proto(config))
}

/// `CoordinatorService.TranscriptionSetConfig` -- write the settings, return them
/// as they now stand.
///
/// An absent key leaves the stored one alone; an empty one clears it, because
/// an operator who deletes the field is clearing the credential, not asking the
/// coordinator to keep handing it out.
pub async fn handle_transcription_set_config(
    core: &CoordCore,
    caller: &Caller,
    request: proto::TranscriptionSetConfigRequest,
) -> ServiceResult<proto::TranscriptionConfig> {
    require_account_device(caller)?;
    let scope = tenant_scope(core)?;
    let config = transcription::store_config(
        core.services.db.pool(),
        scope,
        request.deepgram_key.as_deref(),
        &request.deepgram_language,
    )
    .await
    .map_err(refuse_store)?;
    info!(
        target: "transcription",
        deepgram_configured = config.deepgram_configured,
        language = &config.deepgram_language,
        "config set"
    );
    ok_response(config_proto(config))
}

/// `CoordinatorService.TranscriptionGrantToken` -- the configured key, for an
/// authenticated browser only.
pub async fn handle_transcription_grant_token(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::TranscriptionGrantTokenRequest,
) -> ServiceResult<proto::TranscriptionGrantTokenResponse> {
    require_account_device(caller)?;
    let scope = tenant_scope(core)?;
    let key = transcription::stored_key(core.services.db.pool(), scope)
        .await
        .map_err(|error| refuse_handoff(&error))?
        .ok_or_else(|| {
            ConnectError::new(ErrorCode::FailedPrecondition, "Deepgram not configured")
        })?;
    ok_response(proto::TranscriptionGrantTokenResponse {
        access_token: key,
        expires_in: 0,
        ..Default::default()
    })
}

/// `CoordinatorService.TranscriptionTest` -- is the stored key usable?
///
/// Never fails as an RPC: the answer to "can Deepgram use this key" is `ok`
/// plus a reason, and a caller that treated a refused key as a transport error
/// would offer a retry that cannot help.
pub async fn handle_transcription_test(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::TranscriptionTestRequest,
) -> ServiceResult<proto::TranscriptionTestResponse> {
    require_account_device(caller)?;
    let scope = tenant_scope(core)?;
    let Some(key) = transcription::stored_key(core.services.db.pool(), scope)
        .await
        .map_err(refuse_store)?
    else {
        return ok_response(proto::TranscriptionTestResponse {
            ok: false,
            error: "No Deepgram key saved".to_owned(),
            ..Default::default()
        });
    };

    let outcome = core
        .services
        .telemetry
        .transcription
        .probe_provider(key)
        .await;
    let (ok, error) = match &outcome {
        ProviderProbe::Reachable { .. } => (true, String::new()),
        other => (false, other.failure_reason().unwrap_or_default()),
    };
    ok_response(proto::TranscriptionTestResponse {
        ok,
        error,
        ..Default::default()
    })
}

/// The tenant whose dashboard every transcription row is stamped with.
///
/// Read at call time from the boot facts, so a coordinator that has not booted
/// one is told which fact is missing instead of being handed a scope to write
/// into.
fn tenant_scope(core: &CoordCore) -> Result<&str, ConnectError> {
    Ok(core.services.boot.require_tenant()?.default_dashboard())
}

/// The stored config, as the wire's shape.
fn config_proto(config: transcription::TranscriptionConfig) -> proto::TranscriptionConfig {
    proto::TranscriptionConfig {
        deepgram_configured: config.deepgram_configured,
        deepgram_key_masked: config.deepgram_key_masked,
        deepgram_language: config.deepgram_language,
        ..Default::default()
    }
}


/// A settings row that could not be read or written is the coordinator's fault,
/// and the browser cannot fix it by retrying with a different key.
fn refuse_store(error: TranscriptionStoreError) -> ConnectError {
    ConnectError::new(ErrorCode::Internal, error.to_string())
}

/// Why a handoff is refused, on the wire.
///
/// Not configured is `FailedPrecondition` because the browser must stop asking
/// and show the settings pane instead; anything else is `Unavailable` because
/// the same ask may well succeed once the store does.
fn refuse_handoff(error: &TranscriptionStoreError) -> ConnectError {
    match error {
        TranscriptionStoreError::NotConfigured => {
            ConnectError::new(ErrorCode::FailedPrecondition, "Deepgram not configured")
        }
        other => ConnectError::new(
            ErrorCode::Unavailable,
            format!("Deepgram key handoff failed ({other})"),
        ),
    }
}

/// The Connect method each handler answers, and the function that answers it.
///
/// The lead's list: every row is one arm of the single `impl CoordinatorService`
/// block in `rpc/service_impl.rs`, so wiring this domain is reading the table
/// rather than matching on names by hand. All four are Device: a machine
/// caller is not the operator whose credentials these are.
pub const METHOD_HANDLERS: &[(&str, &str)] = &[
    (
        "TranscriptionGetConfig",
        "diagnostics::rpc_transcription::handle_transcription_get_config",
    ),
    (
        "TranscriptionSetConfig",
        "diagnostics::rpc_transcription::handle_transcription_set_config",
    ),
    (
        "TranscriptionGrantToken",
        "diagnostics::rpc_transcription::handle_transcription_grant_token",
    ),
    (
        "TranscriptionTest",
        "diagnostics::rpc_transcription::handle_transcription_test",
    ),
];
