//! The pre-hello half of one worker link: the forced first frame and the bound
//! it is read under, the in-band credential refresh, and the capabilities a
//! hello is acknowledged with.
//!
//! Called by `worker_link::connection` (the read loop) and
//! `worker_link::link_session` (hello admission). Ports the `hello` and
//! `refreshJwt` arms of `apps/coord/src/workers/worker-conn.ts:231-386`.

use std::collections::BTreeSet;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use roost_protocol::versioning::{
    CAPABILITY_TERMINAL_METADATA_V1, CAPABILITY_TERMINAL_VIEW_OWNER_V1,
};

use crate::auth::authenticate::Authenticator;
use crate::auth::jwt_verify::VerifyClock;
use crate::coord_core::worker_lifecycle::WorkerLifecycle;
use crate::services::CoordServices;
use crate::worker_link::dispatch::FrameClass;
use crate::worker_link::upgrade_admission::VerifiedWorkerCaller;
use crate::worker_link::upstream_frame::{HelloFrame, LinkFrame, decode_link_frame, message_bytes};

/// How long an admitted socket may stay silent before its hello.
///
/// 120 s, Bun's default WebSocket `idleTimeout`, which v2's listener never
/// overrides (`bun-coordinator-listeners.ts:189` sets only the payload cap):
/// that idle reap is what ended a v2 socket that never spoke. The application
/// ping only starts at the hello, so before it this bound is the only thing
/// that ends a half-open pre-hello socket.
pub(super) const PRE_HELLO_WAIT: Duration = Duration::from_secs(120);

/// Read until the forced first frame, under the pre-hello bound.
///
/// `None` means the socket must close with no code: it ended, went silent, sent
/// a hello for another fingerprint, sent a durable event before any hello
/// (v2 `event_before_hello`), or presented a refresh that did not verify.
/// Undecodable bytes and frames that may not cross the barrier are dropped and
/// the read continues, as v2 drops them.
pub(super) async fn read_hello(
    socket: &mut WebSocket,
    services: &CoordServices,
    caller: &VerifiedWorkerCaller,
) -> Option<HelloFrame> {
    let worker_fp = caller.fingerprint.as_str();
    let deadline = tokio::time::Instant::now() + PRE_HELLO_WAIT;
    loop {
        let message = match tokio::time::timeout_at(deadline, socket.recv()).await {
            Err(_) => {
                tracing::warn!(
                    worker_fp,
                    "worker link: no hello inside the pre-hello bound"
                );
                return None;
            }
            Ok(None | Some(Ok(Message::Close(_)))) => {
                tracing::info!(worker_fp, "worker link: closed before its hello");
                return None;
            }
            Ok(Some(Err(error))) => {
                tracing::info!(worker_fp, %error, "worker link: failed before its hello");
                return None;
            }
            Ok(Some(Ok(message))) => message,
        };
        let Some(bytes) = message_bytes(message) else {
            continue;
        };
        let frame = match decode_link_frame(&bytes) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(worker_fp, %error, "worker link: decode_failed; the frame is ignored");
                continue;
            }
        };
        let kind = frame.kind();
        match frame {
            LinkFrame::Hello(hello) if hello.worker_fp.as_str() == worker_fp => return Some(hello),
            LinkFrame::Hello(hello) => {
                tracing::warn!(
                    expected = worker_fp,
                    got = %hello.worker_fp,
                    "worker link: hello_fp_mismatch; closing"
                );
                return None;
            }
            LinkFrame::RefreshJwt { jwt } => {
                if !credential_refresh_accepted(services, worker_fp, &jwt).await {
                    return None;
                }
            }
            LinkFrame::Dispatch(frame) if frame.class == FrameClass::Durable => {
                tracing::warn!(worker_fp, "worker link: event_before_hello; closing");
                return None;
            }
            LinkFrame::Pong { .. } | LinkFrame::Dispatch(_) => {
                tracing::debug!(
                    worker_fp,
                    frame = kind,
                    "worker link: a frame before the hello was dropped"
                );
            }
        }
    }
}

/// Verify an in-band replacement credential for this socket's worker.
///
/// It must verify, resolve to a WORKER principal, and name the fingerprint the
/// upgrade authenticated; `Authenticator` re-checks the key generation itself
/// (`worker-conn.ts:346-385`). Any failure closes the socket, and the worker
/// reconnects with a fresh credential.
pub(super) async fn credential_refresh_accepted(
    services: &CoordServices,
    worker_fp: &str,
    jwt: &str,
) -> bool {
    let Ok(config) = services.boot.require_config() else {
        tracing::warn!(
            worker_fp,
            "worker link: refresh_jwt_failed; no boot config to verify against"
        );
        return false;
    };
    let refreshed = Authenticator {
        database: &services.db,
        keys: &services.jwt_keys,
        clock: VerifyClock::at(crate::rpc::service::now_ms()),
        jwt_max_age_secs: config.jwt_max_age_secs,
    }
    .authenticate(jwt)
    .await;
    match refreshed {
        Err(failure) => {
            tracing::warn!(worker_fp, %failure, "worker link: refresh_jwt_failed; closing");
            false
        }
        Ok(refreshed)
            if refreshed.fingerprint != worker_fp
                || !refreshed.principal.is_worker()
                || refreshed.principal.fingerprint() != worker_fp =>
        {
            tracing::warn!(
                expected_fp = worker_fp,
                got_fp = %refreshed.fingerprint,
                "worker link: refresh_jwt_principal_mismatch; closing"
            );
            false
        }
        Ok(refreshed) => {
            tracing::debug!(
                worker_fp,
                valid_until_ms = refreshed.valid_until_ms,
                "worker link: jwt_refreshed"
            );
            true
        }
    }
}

/// The capabilities a hello is acknowledged with, in v2's order.
///
/// The coordinator's own two first — semantic terminal metadata, which
/// `live_frames` serves, and view ownership, which the view hub serves — then
/// whatever the registered lifecycle owners serve. A capability no owner serves
/// is never acknowledged (`worker-conn.ts:293-321`).
pub(super) fn acknowledged_capabilities(
    advertised: &[String],
    lifecycle: &WorkerLifecycle,
) -> Vec<String> {
    let advertised: BTreeSet<String> = advertised.iter().cloned().collect();
    [
        CAPABILITY_TERMINAL_METADATA_V1,
        CAPABILITY_TERMINAL_VIEW_OWNER_V1,
    ]
    .into_iter()
    .filter(|capability| advertised.contains(*capability))
    .chain(lifecycle.acknowledged_capabilities(&advertised))
    .map(str::to_owned)
    .collect()
}
