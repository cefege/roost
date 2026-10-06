//! The session grant RPC for direct terminal transports: validates the request
//! against the authenticated tab, has the grant owner mint and install the
//! lease, and answers which direct carriers the worker generation supports.
//! Called by `rpc::service_impl`'s `SessionsGrantLocalTerminal` arm; the route
//! authorizer is shared with peer signaling so the two cannot drift apart.
//! Ports `apps/coord/src/terminal/direct/local-terminal-grants.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode, Response, ServiceResult};
use roost_proto::{SessionsGrantLocalTerminalRequest, SessionsGrantLocalTerminalResponse};
use roost_protocol::terminal_capture::TerminalCaptureErrorCode as Failure;
use roost_protocol::terminal_peer::peer::TERMINAL_PEER_MAX_SESSIONS_PER_GRANT;
use roost_protocol::versioning::{
    CAPABILITY_TERMINAL_INPUT_ROUTE_V1, CAPABILITY_TERMINAL_PEER_WEBRTC_V1,
};

use crate::auth::principal::{device_refusal, require_account_device};
use crate::coord_core::{Caller, CoordCore};
use crate::db::{CoordDb, IN_LIST_CHUNK, SqlBuilder, push_in_list};
use crate::terminal_direct::grant_state::{
    LOCAL_TERMINAL_GRANT_TTL_MS, TerminalGrantAuthorization, TerminalGrantRequest,
};

const TERMINAL_GRANT_STRING_MAX_UTF8_BYTES: usize = 128;

/// One grant request, validated.
struct CheckedGrantRequest {
    worker_fp: String,
    tab_id: String,
    session_ids: Vec<String>,
}

/// Mint (or renew) the caller's grant on one worker and report what it may use.
pub async fn handle_sessions_grant_local_terminal(
    core: &CoordCore,
    caller: &Caller,
    request: SessionsGrantLocalTerminalRequest,
) -> ServiceResult<SessionsGrantLocalTerminalResponse> {
    let device_fingerprint = require_account_device(caller)?.to_owned();
    let owner_key = caller
        .principal
        .capture_owner_key()
        .ok_or_else(device_refusal)?;
    let checked = checked_grant_request(&request, caller.tab_id.as_deref())?;
    let database = core.services.db.clone();
    let authorized_worker = checked.worker_fp.clone();
    let authorize: TerminalGrantAuthorization = Arc::new(move |session_ids: Vec<String>| {
        let database = database.clone();
        let worker_fp = authorized_worker.clone();
        Box::pin(async move {
            authorize_terminal_grant_sessions(&database, &worker_fp, &session_ids).await
        })
    });
    let direct = &core.services.terminal_direct;
    let granted = direct
        .grants()
        .grant(TerminalGrantRequest {
            owner_key,
            device_fingerprint,
            tab_id: checked.tab_id,
            worker_fp: checked.worker_fp,
            session_ids: checked.session_ids,
            authorize,
        })
        .result()
        .await
        .map_err(|error| {
            if is_grant_client_failure(&error) {
                error
            } else {
                grant_install_failure(&error)
            }
        })?;
    let lease = &granted.lease;
    let worker_epoch = lease.worker_epoch.clone().unwrap_or_default();
    let epoch_aware = !worker_epoch.is_empty();
    let capabilities = &lease.worker_handle.capabilities;
    let settings = direct.negotiations().settings();
    let peer_supported = epoch_aware
        && settings.enabled
        && capabilities.contains(CAPABILITY_TERMINAL_PEER_WEBRTC_V1);
    let input_route_supported =
        epoch_aware && capabilities.contains(CAPABILITY_TERMINAL_INPUT_ROUTE_V1);
    Response::ok(SessionsGrantLocalTerminalResponse {
        grant_id: lease.grant_id.clone(),
        secret: granted.secret,
        ttl_ms: LOCAL_TERMINAL_GRANT_TTL_MS,
        worker_epoch,
        peer_supported,
        stun_urls: if peer_supported {
            settings.stun_urls.clone()
        } else {
            Vec::new()
        },
        input_route_supported,
        ..Default::default()
    })
}

/// Confirm every requested session is still an open route on the named, not
/// deleted worker row.
pub async fn authorize_terminal_grant_sessions(
    database: &CoordDb,
    worker_fp: &str,
    session_ids: &[String],
) -> Result<(), ConnectError> {
    require_bounded_string(worker_fp, "worker_fp")?;
    require_unique_sessions(session_ids)?;
    let mut rows: Vec<(String, String, String)> = Vec::with_capacity(session_ids.len());
    for chunk in session_ids.chunks(IN_LIST_CHUNK) {
        let mut statement = SqlBuilder::new(
            "SELECT session.id, session.worker_fp, session.status FROM sessions AS session \
             INNER JOIN workers AS worker ON worker.fp = session.worker_fp \
             WHERE worker.deleted_at_ms IS NULL AND session.id IN ",
        );
        push_in_list(&mut statement, chunk);
        let chunk_rows = statement
            .build_query_as::<(String, String, String)>()
            .fetch_all(database.pool())
            .await
            .map_err(|error| {
                tracing::error!(worker_fp, %error, "terminal grant: the session route lookup failed");
                ConnectError::new(ErrorCode::Internal, "terminal grant authorization failed")
            })?;
        rows.extend(chunk_rows);
    }
    let routes: HashMap<&str, (&str, &str)> = rows
        .iter()
        .map(|(id, route_worker, status)| (id.as_str(), (route_worker.as_str(), status.as_str())))
        .collect();
    for session_id in session_ids {
        match routes.get(session_id.as_str()) {
            Some((_, status)) if *status != "open" => {
                return Err(capture_failure(Failure::SessionUnknown, "session_ids"));
            }
            None => return Err(capture_failure(Failure::SessionUnknown, "session_ids")),
            Some((route_worker, _)) if *route_worker != worker_fp => {
                return Err(capture_failure(Failure::PermissionDenied, "worker_fp"));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// v2 `captureFailure`: the bounded `"<code>: <field>"` refusal whose Connect
/// code the capture vocabulary fixes.
#[must_use]
pub fn capture_failure(code: Failure, field: &str) -> ConnectError {
    let (name, connect_code) = match code {
        Failure::InvalidArgument => ("invalid_argument", ErrorCode::InvalidArgument),
        Failure::EvidenceTooLarge => ("evidence_too_large", ErrorCode::InvalidArgument),
        Failure::EvidenceMalformed => ("evidence_malformed", ErrorCode::InvalidArgument),
        Failure::PermissionDenied => ("permission_denied", ErrorCode::PermissionDenied),
        Failure::SessionUnknown => ("session_unknown", ErrorCode::NotFound),
        Failure::LeaseConflict => ("lease_conflict", ErrorCode::AlreadyExists),
        Failure::LeaseExpired => ("lease_expired", ErrorCode::FailedPrecondition),
        Failure::LeaseAbsent => ("lease_absent", ErrorCode::FailedPrecondition),
        Failure::CaptureExpired => ("capture_expired", ErrorCode::FailedPrecondition),
        Failure::CaptureInFlight => ("capture_in_flight", ErrorCode::Aborted),
        Failure::RateLimited => ("rate_limited", ErrorCode::ResourceExhausted),
        Failure::ResourceExhausted => ("resource_exhausted", ErrorCode::ResourceExhausted),
        Failure::WorkerOffline => ("worker_offline", ErrorCode::Unavailable),
        Failure::WorkerTimeout => ("worker_timeout", ErrorCode::DeadlineExceeded),
        Failure::WorkerFailed => ("worker_failed", ErrorCode::Internal),
        Failure::StorageFailed => ("storage_failed", ErrorCode::Internal),
        Failure::Internal => ("internal", ErrorCode::Internal),
    };
    ConnectError::new(connect_code, format!("{name}: {field}"))
}

fn checked_grant_request(
    request: &SessionsGrantLocalTerminalRequest,
    authenticated_tab_id: Option<&str>,
) -> Result<CheckedGrantRequest, ConnectError> {
    require_bounded_string(&request.worker_fp, "worker_fp")?;
    require_bounded_string(&request.tab_id, "tab_id")?;
    let authenticated_tab_id =
        authenticated_tab_id.ok_or_else(|| capture_failure(Failure::InvalidArgument, "tab_id"))?;
    require_bounded_string(authenticated_tab_id, "tab_id")?;
    if request.tab_id != authenticated_tab_id {
        return Err(capture_failure(Failure::PermissionDenied, "tab_id"));
    }
    require_unique_sessions(&request.session_ids)?;
    Ok(CheckedGrantRequest {
        worker_fp: request.worker_fp.clone(),
        tab_id: request.tab_id.clone(),
        session_ids: request.session_ids.clone(),
    })
}

/// One to 256 bounded, distinct session ids.
fn require_unique_sessions(session_ids: &[String]) -> Result<(), ConnectError> {
    if session_ids.is_empty() || session_ids.len() > TERMINAL_PEER_MAX_SESSIONS_PER_GRANT {
        return Err(capture_failure(Failure::InvalidArgument, "session_ids"));
    }
    let mut seen = HashSet::with_capacity(session_ids.len());
    for session_id in session_ids {
        require_bounded_string(session_id, "session_ids")?;
        if !seen.insert(session_id.as_str()) {
            return Err(capture_failure(Failure::InvalidArgument, "session_ids"));
        }
    }
    Ok(())
}

fn require_bounded_string(value: &str, field: &str) -> Result<(), ConnectError> {
    if value.is_empty() || value.len() > TERMINAL_GRANT_STRING_MAX_UTF8_BYTES {
        return Err(capture_failure(Failure::InvalidArgument, field));
    }
    Ok(())
}

/// A refusal the browser caused, which passes through unchanged.
fn is_grant_client_failure(error: &ConnectError) -> bool {
    matches!(
        error.code,
        ErrorCode::InvalidArgument
            | ErrorCode::NotFound
            | ErrorCode::PermissionDenied
            | ErrorCode::ResourceExhausted
    )
}

/// Anything else is the worker's install failing, in capture vocabulary.
fn grant_install_failure(error: &ConnectError) -> ConnectError {
    match error.code {
        ErrorCode::Unavailable => capture_failure(Failure::WorkerOffline, "worker_fp"),
        ErrorCode::DeadlineExceeded => capture_failure(Failure::WorkerTimeout, "worker_fp"),
        _ => capture_failure(Failure::WorkerFailed, "worker_fp"),
    }
}
