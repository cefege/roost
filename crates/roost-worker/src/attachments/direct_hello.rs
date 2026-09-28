//! Hello admission and the authority lifetime of an admitted direct upload:
//! the grant verdict, the one-carrier-per-grant fence, the hello deadline of
//! a loopback socket, the transfer lease's clock, and the grant changes that
//! fence an admitted upload at once. Called by `direct_sockets`. Ports
//! `acceptHello`, `handleGrantChange` and the timers of
//! `apps/worker/src/attachments/attachment-direct-socket.ts`.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use roost_proto::AttachmentTransferHello;
use roost_protocol::attachment_transfer::{
    HELLO_DEADLINE_MS, MAX_ACTIVE_PER_WORKER, TransferErrorReason,
};
use tokio::task::AbortHandle;

use super::direct_frames::{AttachmentReadyFields, send_attachment_ready};
use super::direct_session::{AttachmentPortSession, Refusal};
use super::direct_sockets::{AttachmentDirectSockets, Inner};
use super::grants::{GrantChange, GrantRemovalReason};
use super::transfer_admission::{
    AttachmentPeerExpectedTuple, admit_attachment_transfer_hello, attachment_metadata_matches_grant,
};
use super::transfer_lease::AttachmentTransferLease;

impl AttachmentDirectSockets {
    /// Grant verification runs outside the lock: a lapsed grant is removed
    /// inside `verify`, and the store tells this receiver about it at once.
    pub(super) fn accept_hello(
        &self,
        socket_id: &str,
        serial: u64,
        hello: &AttachmentTransferHello,
        expected_peer: Option<&AttachmentPeerExpectedTuple>,
    ) {
        let admission =
            admit_attachment_transfer_hello(hello, &self.inner.deps.grants, expected_peer);
        let deps = &self.inner.deps;
        let mut state = self.inner.lock();
        let Some(session) = state.session(socket_id, serial) else {
            return;
        };
        if session.terminal || session.metadata.is_some() {
            return;
        }
        let metadata = match admission {
            Ok(metadata) => metadata,
            Err(reason) => {
                state.fail(&deps.operations, socket_id, serial, Refusal::bare(reason));
                return;
            }
        };
        let replayed = state.sessions.values().any(|other| {
            other.serial != serial
                && other
                    .metadata
                    .as_ref()
                    .is_some_and(|admitted| admitted.grant_id == metadata.grant_id)
        });
        if replayed {
            // One grant authorizes one live carrier: a replay can neither
            // multiply slots nor steal the upload.
            let refusal = Refusal::bare(TransferErrorReason::GrantUnavailable);
            state.fail(&deps.operations, socket_id, serial, refusal);
            return;
        }
        let admitted = state.count_sessions(AttachmentPortSession::holds_admitted_slot);
        if expected_peer.is_none() && admitted >= MAX_ACTIVE_PER_WORKER {
            if let Some(mut session) = state.remove_session(socket_id, serial) {
                session.terminal = true;
                session
                    .port
                    .close(Some(1013), "attachment transfer capacity is full");
                tracing::info!("attachment transfer capacity refused an admitted hello");
            }
            return;
        }
        let Some(session) = state.session(socket_id, serial) else {
            return;
        };
        if let Some(timer) = session.setup_timer.take() {
            timer.abort();
        }
        let port = Arc::clone(&session.port);
        let ready = AttachmentReadyFields {
            worker_fingerprint: &deps.worker_fingerprint,
            worker_epoch: &deps.worker_epoch,
            session_id: &metadata.session_id,
            upload_id: &metadata.upload_id,
        };
        let sent = send_attachment_ready(port.as_ref(), ready);
        session.metadata = Some(metadata);
        if !sent {
            state.retire_unacknowledged_route(socket_id, serial, &deps.operations, "write_failed");
            return;
        }
        port.mark_authenticated();
        let lease = AttachmentTransferLease::start(Instant::now());
        session.lease_timer = Some(arm_lease_timer(
            &self.inner,
            socket_id.to_owned(),
            serial,
            lease.deadline(),
        ));
        session.lease = Some(lease);
        let (carrier, active) = (port.kind().as_str(), state.sessions.len());
        tracing::info!(carrier, active, "attachment socket authenticated");
    }
}

/// Expiry gates a fresh hello only; replacement or explicit removal of the
/// grant an upload was admitted under is an immediate fence.
pub(super) fn handle_grant_change(inner: &Inner, change: &GrantChange) {
    let (grant, removed) = match change {
        GrantChange::Removed {
            reason: GrantRemovalReason::Expired,
            ..
        } => return,
        GrantChange::Removed { grant, .. } => (grant, true),
        GrantChange::Installed { grant, .. } => (grant, false),
    };
    let mut state = inner.lock();
    let fenced: Vec<(String, u64)> = state
        .sessions
        .iter()
        .filter(|(_, session)| {
            session.metadata.as_ref().is_some_and(|metadata| {
                metadata.grant_id == grant.grant_id
                    && (removed || !attachment_metadata_matches_grant(metadata, grant))
            })
        })
        .map(|(socket_id, session)| (socket_id.clone(), session.serial))
        .collect();
    for (socket_id, serial) in fenced {
        let refusal = Refusal::bare(TransferErrorReason::GrantUnavailable);
        state.fail(&inner.deps.operations, &socket_id, serial, refusal);
    }
}

pub(super) fn arm_hello_deadline(
    inner: &Arc<Inner>,
    socket_id: String,
    serial: u64,
) -> AbortHandle {
    let weak = Arc::downgrade(inner);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(HELLO_DEADLINE_MS)).await;
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let mut state = inner.lock();
        let Some(session) = state.session(&socket_id, serial) else {
            return;
        };
        session.setup_timer = None;
        if session.metadata.is_none() {
            tracing::info!("attachment socket hello deadline elapsed");
            let refusal = Refusal::bare(TransferErrorReason::InvalidHello);
            state.fail(&inner.deps.operations, &socket_id, serial, refusal);
        }
    })
    .abort_handle()
}

/// The lease's own clock: it re-reads the deadline valid activity moved, and
/// fails the upload once the lease reports expiry.
fn arm_lease_timer(
    inner: &Arc<Inner>,
    socket_id: String,
    serial: u64,
    first_deadline: Instant,
) -> AbortHandle {
    let weak: Weak<Inner> = Arc::downgrade(inner);
    tokio::spawn(async move {
        let mut deadline = first_deadline;
        loop {
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let mut state = inner.lock();
            let Some(session) = state.session(&socket_id, serial) else {
                return;
            };
            let Some(lease) = session.lease.as_mut() else {
                return;
            };
            if lease.allows_activity(Instant::now()) {
                deadline = lease.deadline();
                continue;
            }
            tracing::info!("attachment transfer lease expired");
            let refusal = Refusal::bare(TransferErrorReason::GrantUnavailable);
            state.fail(&inner.deps.operations, &socket_id, serial, refusal);
            return;
        }
    })
    .abort_handle()
}
