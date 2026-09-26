//! The pairing RPC surface: an anonymous browser asks, an operator decides,
//! the requester confirms. Seven `Pair*` methods, each a `handle_*` the
//! service impl delegates to.
//!
//! Ported from `apps/coord/src/auth/handlers-pairing.ts`. The facade that
//! mounted it, `apps/coord/src/auth/handlers-auth.ts`, has no coordinator-side
//! equivalent here: it exists only to keep v2's router auth-handler object in
//! one literal, and `service_impl.rs` is already the one place these methods
//! live.
//!
//! THE THREE PUBLIC METHODS TAKE THE REQUEST, NOT A CALLER. `PairCreate`,
//! `PairPoll` and `PairConfirm` are `AuthRequirement::Public`, and the auth
//! gate deliberately stores no `Caller` for a request that presented no
//! credential (`rpc/auth_gate.rs:107-116`) -- so `caller_of` would answer every
//! one of them with a wiring-fault refusal, and a fresh install could never
//! pair. They take the [`RequestContext`] instead, because what they need from
//! the request is its headers and its peer address: provenance is
//! server-observed metadata, which belongs with the request rather than with an
//! identity.
//!
//! THE FOUR AUTHORIZED METHODS TAKE A `Caller`, as every other domain's
//! handlers do. Three of them also accept a direct on-host operator with no
//! device key at all -- the "approve from the machine you are sitting at" path,
//! which is the only way the first browser of a fresh install can be approved.
//!
//! NO SECRET IS EVER LOGGED, FORMATTED INTO AN ERROR, OR PUT IN A RESPONSE.
//! The requester token and the verification code are validated, digested and
//! dropped inside these functions. Every `tracing` line below names an
//! `ephemeral_id`, a fingerprint, a label, a country, or a status -- never a
//! code, a token, or a digest of either.

use connectrpc::RequestContext;
use connectrpc::{Response, ServiceResult};

use crate::auth::authorized_keys::fingerprint_of_raw_public_key;
use crate::auth::pairing::account::{self, CreateOutcome, PairRequestCreate};
use crate::auth::pairing::confirmation;
use crate::auth::pairing::rows;
use crate::auth::pairing::rpc_support::{
    approver_or_on_host, caller_origin_of, deny_request, front_door_identity, lease,
    list_pending, optional_fingerprint, publish_pending, publish_removed, read_status_facts,
    read_under_token, report_confirmation,
};
use crate::auth::pairing::secrets::{
    self, PAIR_REQUEST_TTL_MS, decode_ed25519_pubkey, normalize_pair_request_id,
    normalize_pair_requester_token, normalize_pair_verification_code, pairing_secret_digest,
};
use crate::auth::pairing::status::{ApprovalAuthority, ApprovalOutcome, StoredStatus};
use crate::auth::pairing::{PairingError, PairingRefusal, authority};
use crate::coord_core::{Caller, CoordCore};

/// The Connect method each handler answers, and the function that answers it.
///
/// The integrator's list: every row is one arm of the single `impl
/// CoordinatorService` block in `rpc/service_impl.rs`.
pub const METHOD_HANDLERS: [(&str, &str); 7] = [
    ("PairCreate", "auth::rpc_pairing::handle_pair_create"),
    ("PairPoll", "auth::rpc_pairing::handle_pair_poll"),
    ("PairList", "auth::rpc_pairing::handle_pair_list"),
    ("PairApprove", "auth::rpc_pairing::handle_pair_approve"),
    ("PairConfirm", "auth::rpc_pairing::handle_pair_confirm"),
    ("PairDeny", "auth::rpc_pairing::handle_pair_deny"),
    (
        "PairApprovalStatus",
        "auth::rpc_pairing::handle_pair_approval_status",
    ),
];

/// An anonymous browser asks to pair, and the coordinator remembers what it
/// observed about the requester.
pub async fn handle_pair_create(
    core: &CoordCore,
    context: &RequestContext,
    request: roost_proto::PairCreateRequest,
) -> ServiceResult<roost_proto::PairCreateResponse> {
    secrets::assert_pairing_ceremony_version(request.ceremony_version)
        .map_err(PairingError::into_error)?;
    let ephemeral_id =
        normalize_pair_request_id(&request.ephemeral_id).map_err(PairingError::into_error)?;
    let requester_token = normalize_pair_requester_token(&request.requester_token)
        .map_err(PairingError::into_error)?;
    let public_key =
        decode_ed25519_pubkey(&request.ssh_pubkey_b64).map_err(PairingError::into_error)?;
    let _lease = lease(core)?;
    let now_ms = crate::rpc::service::now_ms();
    let origin = caller_origin_of(context);
    let edge = front_door_identity(core, context, origin.on_host).await?;
    let requester_token_hash = pairing_secret_digest(requester_token);
    let observed = crate::auth::pairing::provenance::capture_pair_request_provenance(
        context.headers(),
        &origin,
    );
    let input = PairRequestCreate {
        ephemeral_id,
        requester_token_hash: &requester_token_hash,
        public_key,
        label: &request.label,
        now_ms,
        expires_at_ms: now_ms + PAIR_REQUEST_TTL_MS,
        provenance: &observed,
        edge_identity_provider: edge.as_ref().map(|identity| identity.provider()),
        edge_identity: edge.as_ref().map(|identity| identity.email()),
    };
    let creation = account::create_pair_request(&core.services.db, &input)
        .await
        .map_err(PairingError::into_error)?;
    // Rust has no `enum.field`, so the expired set comes from
    // `CreateOutcome::expired_ids` -- which is where that match belongs, once,
    // rather than here in a caller that would have to be right about the shape
    // of a type it does not own. All three outcomes carry a set, so all three
    // owe the bus the same `removed` frames, and every id is reclaimed before
    // the ceremony decides what this call was.
    for removed in creation.expired_ids() {
        publish_removed(core, removed);
    }
    // Which of the three it was is the ceremony's answer, not a caller's: a
    // retry and a fresh create are indistinguishable over the wire, and only
    // the expiry is a refusal.
    match &creation {
        CreateOutcome::Expired { .. } => return Err(PairingRefusal::Expired.into_error()),
        CreateOutcome::Retry { .. } => return Response::ok(created(ephemeral_id)),
        CreateOutcome::Created { .. } => {}
    }
    publish_pending(
        core,
        ephemeral_id,
        &request.label,
        now_ms,
        &observed,
        edge.as_ref(),
    );
    tracing::info!(
        ephemeral_id,
        label = %request.label,
        client_ip = %observed.source_ip(),
        country_code = ?observed.country_code,
        edge_identity_verified = edge.is_some(),
        "a browser posted a pairing request"
    );
    Response::ok(created(ephemeral_id))
}

/// The requester asks how its request is doing, and the answer is the only
/// thing its token unlocks.
pub async fn handle_pair_poll(
    core: &CoordCore,
    _context: &RequestContext,
    request: roost_proto::PairPollRequest,
) -> ServiceResult<roost_proto::PairPollResponse> {
    secrets::assert_pairing_ceremony_version(request.ceremony_version)
        .map_err(PairingError::into_error)?;
    let ephemeral_id =
        normalize_pair_request_id(&request.ephemeral_id).map_err(PairingError::into_error)?;
    let requester_token = normalize_pair_requester_token(&request.requester_token)
        .map_err(PairingError::into_error)?;
    let now_ms = crate::rpc::service::now_ms();
    let row = read_under_token(
        &core.services.db,
        ephemeral_id,
        &pairing_secret_digest(requester_token),
    )
    .await?;
    // Expiry is answered from the read, never written here: this is a 1 Hz poll
    // from a browser that is waiting, and a poll that wrote would turn a
    // read-only question into a durable mutation on somebody else's cadence.
    let stored = StoredStatus::parse(&row.status)
        .map_err(|error| PairingError::from(error).into_error())?;
    let status = match stored {
        StoredStatus::Pending | StoredStatus::VerificationRequired
            if row.expires_at_ms <= now_ms =>
        {
            StoredStatus::Expired
        }
        settled => settled,
    };
    Response::ok(roost_proto::PairPollResponse {
        status: status.as_wire().to_string(),
        expires_at_ms: u64::try_from(row.expires_at_ms).unwrap_or_default(),
        ..Default::default()
    })
}

/// An approver lists the requests waiting on them.
///
/// Device-authenticated, and also reachable by a direct on-host operator with
/// no device key: v2's `pairList` takes either, and an operator approving the
/// first browser of a fresh install has, by definition, no browser.
pub async fn handle_pair_list(
    core: &CoordCore,
    caller: &Caller,
    _request: roost_proto::PairListRequest,
) -> ServiceResult<roost_proto::PairListResponse> {
    approver_or_on_host(caller)?;
    let now_ms = crate::rpc::service::now_ms();
    let requests = list_pending(&core.services.db, now_ms).await?;
    Response::ok(roost_proto::PairListResponse {
        requests,
        ..Default::default()
    })
}

/// An approver binds a verification code, and the request moves to
/// `verification_required`.
pub async fn handle_pair_approve(
    core: &CoordCore,
    caller: &Caller,
    request: roost_proto::PairApproveRequest,
) -> ServiceResult<roost_proto::PairApproveResponse> {
    secrets::assert_pairing_ceremony_version(request.ceremony_version)
        .map_err(PairingError::into_error)?;
    let ephemeral_id =
        normalize_pair_request_id(&request.ephemeral_id).map_err(PairingError::into_error)?;
    let verification_code = normalize_pair_verification_code(&request.verification_code)
        .map_err(PairingError::into_error)?;
    let _lease = lease(core)?;
    let approver = approver_or_on_host(caller)?;
    let now_ms = crate::rpc::service::now_ms();
    let account_id = authority::paired_browser_account_id(&core.services.db, approver)
        .await
        .map_err(PairingError::into_error)?
        .ok_or_else(|| PairingRefusal::AccountUnavailable.into_error())?;
    let row = rows::read_pair_request(core.services.db.pool(), ephemeral_id)
        .await
        .map_err(PairingError::into_error)?
        .ok_or_else(|| PairingRefusal::NotFound.into_error())?;
    if !row.speaks_current_ceremony() {
        return Err(PairingRefusal::CeremonyVersion.into_error());
    }
    let live = row
        .live()
        .ok_or_else(|| PairingRefusal::NotPending.into_error())?;
    let approver_identity = ApprovalAuthority {
        account_id,
        approver_fingerprint: approver.map(str::to_string),
    };
    // The code is digested here and nowhere else in the approve path, and the
    // plaintext leaves scope at the end of this function without ever being
    // formatted, stored or logged.
    let outcome = account::apply_approval(
        &core.services.db,
        live,
        &approver_identity,
        &pairing_secret_digest(verification_code),
        now_ms,
    )
    .await
    .map_err(PairingError::into_error)?;
    match outcome {
        ApprovalOutcome::Expired => {
            publish_removed(core, ephemeral_id);
            tracing::info!(ephemeral_id, "a pair request expired before approval");
            Err(PairingRefusal::Expired.into_error())
        }
        ApprovalOutcome::Approved { identity } => {
            publish_removed(core, &identity.ephemeral_id);
            tracing::info!(
                ephemeral_id,
                requester_fp = %fingerprint_of_raw_public_key(&row.public_key),
                "a pair request is awaiting its verification code"
            );
            Response::ok(approved())
        }
        // A retry is the same approval: the row is already
        // `verification_required`, and the bus dropped it from the pending list
        // on the call that moved it.
        ApprovalOutcome::Retry => Response::ok(approved()),
        ApprovalOutcome::Refused(refusal) => Err(refusal.into_error()),
    }
}

/// The requester presents the code, and this is the only call that can turn a
/// pending key into an authorized one.
pub async fn handle_pair_confirm(
    core: &CoordCore,
    _context: &RequestContext,
    request: roost_proto::PairConfirmRequest,
) -> ServiceResult<roost_proto::PairConfirmResponse> {
    secrets::assert_pairing_ceremony_version(request.ceremony_version)
        .map_err(PairingError::into_error)?;
    let ephemeral_id =
        normalize_pair_request_id(&request.ephemeral_id).map_err(PairingError::into_error)?;
    let requester_token = normalize_pair_requester_token(&request.requester_token)
        .map_err(PairingError::into_error)?;
    let verification_code = normalize_pair_verification_code(&request.verification_code)
        .map_err(PairingError::into_error)?;
    let _lease = lease(core)?;
    let now_ms = crate::rpc::service::now_ms();
    let result = confirmation::confirm_pair_request(
        &core.services.db,
        ephemeral_id,
        &pairing_secret_digest(requester_token),
        &pairing_secret_digest(verification_code),
        now_ms,
    )
    .await
    .map_err(|error| {
        match error.refusal() {
            Some(refusal) => {
                tracing::info!(ephemeral_id, refused = %refusal, "a pair confirmation was refused");
                refusal.into_error()
            }
            None => {
                tracing::error!(ephemeral_id, error = %error, "a pair confirmation could not be decided");
                error.into_error()
            }
        }
    })?;
    report_confirmation(core, ephemeral_id, &result);
    Response::ok(roost_proto::PairConfirmResponse {
        ok: result.ok,
        ..Default::default()
    })
}

/// An approver refuses a request outright.
pub async fn handle_pair_deny(
    core: &CoordCore,
    caller: &Caller,
    request: roost_proto::PairDenyRequest,
) -> ServiceResult<roost_proto::PairDenyResponse> {
    let ephemeral_id =
        normalize_pair_request_id(&request.ephemeral_id).map_err(PairingError::into_error)?;
    approver_or_on_host(caller)?;
    let _lease = lease(core)?;
    deny_request(core, ephemeral_id, crate::rpc::service::now_ms()).await?;
    publish_removed(core, ephemeral_id);
    tracing::info!(ephemeral_id, "a pair request was denied");
    Response::ok(roost_proto::PairDenyResponse {
        ok: true,
        ..Default::default()
    })
}

/// The approver reads back how far their approval got, and nothing else.
pub async fn handle_pair_approval_status(
    core: &CoordCore,
    caller: &Caller,
    request: roost_proto::PairApprovalStatusRequest,
) -> ServiceResult<roost_proto::PairApprovalStatusResponse> {
    secrets::assert_pairing_ceremony_version(request.ceremony_version)
        .map_err(PairingError::into_error)?;
    let ephemeral_id =
        normalize_pair_request_id(&request.ephemeral_id).map_err(PairingError::into_error)?;
    // A remote caller without browser authority gets the device-auth marker,
    // so the approver can tell a revoked key from a wrong method. A direct
    // on-host caller stays admitted, which is the whole point of an on-host
    // approval.
    let fingerprint =
        if caller.on_host {
            optional_fingerprint(caller)
        } else {
            Some(
                caller
                    .principal
                    .require_account_device()
                    .map_err(|_| crate::auth::pairing::authentication_required())?,
            )
        };
    let facts = read_status_facts(&core.services.db, ephemeral_id)
        .await
        .map_err(PairingError::into_error)?;
    let status = crate::auth::pairing::status::read_approval_status(
        facts.as_ref(),
        fingerprint,
        caller.on_host,
        crate::rpc::service::now_ms(),
    )
    .map_err(PairingRefusal::into_error)?;
    Response::ok(roost_proto::PairApprovalStatusResponse {
        status: status.to_string(),
        ..Default::default()
    })
}

/// The one field `PairCreate` answers with, on every path that succeeds.
fn created(ephemeral_id: &str) -> roost_proto::PairCreateResponse {
    roost_proto::PairCreateResponse {
        ephemeral_id: ephemeral_id.to_string(),
        ..Default::default()
    }
}

/// The one field `PairApprove` answers with, on every path that succeeds.
fn approved() -> roost_proto::PairApproveResponse {
    roost_proto::PairApproveResponse {
        ok: true,
        ..Default::default()
    }
}
