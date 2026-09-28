//! One admitted Sync WebSocket's loop: flush what the link released, write it,
//! then wait on whichever comes first -- a client frame, a bus wake-up, the
//! keepalive, the reauth deadline, or the ACK deadline.
//!
//! Called by `http::upgrade::sync_upgrade` once the upgrade is admitted; the
//! open and the release are `sync_ws::socket_open`, the shared state is
//! `sync_ws::driver`, the bus listeners `sync_ws::live_feed`, the client frames
//! `sync_ws::ingress`. Ports the `message`/`close` handlers and the keepalive
//! of `apps/coord/src/sync/sync-ws-handler.ts` and the scheduling of
//! `scheduleV2`/`flushV2` in `sync-ws-v2-egress.ts`.
//!
//! ONE TASK WRITES. v2's listeners wrote the socket themselves because Bun
//! buffered for them; here every writer is this loop, so frame order on the
//! wire is the order the link's outbox received them, and a write that blocks
//! blocks the one place that could have produced the next frame.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};

use crate::services::CoordServices;
use crate::sync_ws::commands_layout::settle_layout_result;
use crate::sync_ws::driver::{LinkClose, SOCKET_WRITE_TIMEOUT_MS, now_ms};
use crate::sync_ws::ingress::{IngressEffect, accept_client_frame, refuse_text_frame};
use crate::sync_ws::socket_open::{OpenedSocket, REAUTH, open_socket, release_socket};
use crate::sync_ws::upgrade_admission::{SyncScope, VerifiedSyncCaller};

/// The keepalive cadence every long-lived coordinator socket shares, because
/// an idle timeout resets only on received traffic (`sync-ws-handler.ts:57-60`).
pub const KEEPALIVE_INTERVAL_MS: u64 = 30_000;

/// Serve one admitted Sync socket until it closes.
///
/// `reauth_at_ms` is the credential's hard deadline: an already-passed one
/// closes `4003` before anything is sent, and a future one closes `4003` when
/// it passes. The production upgrade passes `None`, as v2 does
/// (`bun-coordinator-listeners.ts:328`).
pub async fn serve_socket(
    mut socket: WebSocket,
    caller: VerifiedSyncCaller,
    scope: SyncScope,
    reauth_at_ms: Option<i64>,
    services: Arc<CoordServices>,
) {
    let mut opened = match open_socket(&mut socket, &caller, &scope, reauth_at_ms, &services).await
    {
        Ok(opened) => opened,
        Err(close) => {
            tracing::info!(
                event = "sync-ws",
                action = "open_refused",
                caller_fp = %caller.fingerprint,
                code = close.code,
                reason = close.reason,
                "sync socket closed at open"
            );
            write_close(&mut socket, close).await;
            return;
        }
    };
    let close = serve_until_closed(&mut socket, &mut opened, reauth_at_ms, &services).await;
    let socket_id = opened.socket_id.clone();
    release_socket(opened, &services);
    if let Some(close) = close {
        write_close(&mut socket, close).await;
    }
    tracing::info!(
        event = "sync-ws",
        action = "close",
        caller_fp = %caller.fingerprint,
        socket_id = %socket_id,
        code = close.map(|close| close.code),
        "sync socket closed"
    );
}

/// The socket loop. Returns the close to send, or `None` when the peer left.
async fn serve_until_closed(
    socket: &mut WebSocket,
    opened: &mut OpenedSocket,
    reauth_at_ms: Option<i64>,
    services: &Arc<CoordServices>,
) -> Option<LinkClose> {
    let link = Arc::clone(&opened.link);
    let reauth = async move {
        match reauth_at_ms {
            Some(deadline) => crate::auth::ws_auth_deadline::sleep_until_reauth(deadline).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(reauth);
    let period = Duration::from_millis(KEEPALIVE_INTERVAL_MS);
    let mut keepalive = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let now = now_ms();
        let (frames, more, close, ack_wait) = {
            let mut state = link.lock();
            let more = state.flush_turn(now);
            (
                state.outbox.take(),
                more,
                state.close,
                state.ack_deadline_in_ms(now),
            )
        };
        for encoded in frames {
            if let Err(cause) = write_frame(socket, encoded).await {
                let mut state = link.lock();
                state.decide_close(LinkClose::BACKPRESSURE, cause, "write", now_ms());
                return state.close;
            }
        }
        if close.is_some() {
            return close;
        }
        if more {
            // A full batch went out and more is eligible: yield, then run the
            // next turn without waiting for a wake (`sync-ws-v2-egress.ts:355-359`).
            tokio::task::yield_now().await;
            continue;
        }
        let ack_deadline = async move {
            match ack_wait {
                Some(wait_ms) => tokio::time::sleep(Duration::from_millis(wait_ms)).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Binary(bytes))) => {
                    let effect = accept_client_frame(&link, &services.feed, &bytes, now_ms());
                    carry_out(effect, opened, services);
                }
                Some(Ok(Message::Text(_))) => refuse_text_frame(&link, now_ms()),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_)) | Err(_)) | None => return None,
            },
            () = link.woken() => {}
            _ = keepalive.tick() => link.lock().send_keepalive(now_ms()),
            () = &mut reauth => link.lock().decide_close(REAUTH, "reauth", "deadline", now_ms()),
            () = ack_deadline => link.lock().enforce_ack_deadline(now_ms()),
        }
    }
}

/// The parts of a client frame's outcome that reach beyond the link, run with
/// the link unlocked: a subscription and a UI-runtime settlement may each
/// publish to a bus this socket's own listeners lock the link from.
fn carry_out(effect: IngressEffect, opened: &mut OpenedSocket, services: &Arc<CoordServices>) {
    match effect {
        IngressEffect::Nothing => {}
        IngressEffect::AuditSubscription(subscribed) => {
            opened
                .feed
                .set_audit_subscribed(&opened.link, &services.buses, subscribed);
        }
        IngressEffect::LayoutResult(outcome) => {
            settle_layout_result(
                &services.ui_state,
                &opened.layout_context,
                &opened.socket_id,
                &outcome,
            );
        }
    }
}

/// Write one encoded frame, or name why the socket must close: a write that
/// failed dropped the frame, and one that blocked past the timeout is a
/// client that stopped reading (`sync-ws-v1-delivery.ts:196-226`).
pub(in crate::sync_ws) async fn write_frame(
    socket: &mut WebSocket,
    encoded: Vec<u8>,
) -> Result<(), &'static str> {
    let write = socket.send(Message::Binary(encoded.into()));
    match tokio::time::timeout(Duration::from_millis(SOCKET_WRITE_TIMEOUT_MS), write).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("frame_dropped"),
        Err(_) => Err("timeout"),
    }
}

/// Send the close frame, bounded: a peer that stopped reading cannot hold the
/// task open by not accepting it.
async fn write_close(socket: &mut WebSocket, close: LinkClose) {
    let frame = Message::Close(Some(CloseFrame {
        code: close.code,
        reason: close.reason.into(),
    }));
    let bound = Duration::from_millis(SOCKET_WRITE_TIMEOUT_MS);
    if tokio::time::timeout(bound, socket.send(frame))
        .await
        .is_err()
    {
        tracing::debug!(
            event = "sync-ws",
            action = "close_write_timeout",
            code = close.code
        );
    }
}
