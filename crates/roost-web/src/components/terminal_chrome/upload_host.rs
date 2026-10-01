//! The browser half of the upload driver: the coordinator-relay chunk loop, the
//! serial queue that preserves pick order, and the direct-carrier environment
//! that reports what this build can actually open.
//!
//! The loopback socket and the attachment WebRTC transport are the direct
//! carrier drivers, which live in the carrier slice; this file asks them
//! whether a carrier is available and drives whichever one answers. Until one
//! does, the environment reports no route and the relay carries the upload —
//! never a pretended-open carrier.
//! Ports the host half of `apps/web/src/lib/attachments.ts`.

use roost_client_core::client::attachments::direct::relay::{RelayChunk, RelayUpload};
use roost_client_core::client::attachments::direct::{
    AttachmentDirectEnvironment, AttachmentDirectUploadRequest, LocalWorkerDoor, RouteOpen,
};
use roost_client_core::client::attachments::grant::{
    AttachmentDirectGrant, AttachmentDirectGrantRequest, AttachmentDirectGrantResponse,
};
use roost_client_core::client::attachments::transfer::AttachmentTransferResult;
use roost_client_core::client::attachments::transfer::receipt::AttachmentTransferStatus;
use roost_client_core::client::rpc::calls::attachment_direct::{
    GrantAttachmentDirect, ReadAttachmentDirectStatus,
};
use roost_client_core::client::rpc::calls::attachments::{ProbeAttachment, WriteAttachmentChunk};

use super::upload_plan::UploadPlan;
use crate::pump::Pump;

/// The exact authority one upload would run under, when one was minted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingGrant {
    /// The tuple the grant must name, so a grant for a different upload is
    /// refused here rather than admitted by a carrier that would reject it.
    pub request: Option<AttachmentDirectGrantRequest>,
    /// What the coordinator answered, when it answered.
    pub response: Option<AttachmentDirectGrantResponse>,
}

impl PendingGrant {
    /// Record a grant the driver already asked for, keyed by its tuple.
    pub fn record(
        &mut self,
        request: AttachmentDirectGrantRequest,
        response: AttachmentDirectGrantResponse,
    ) {
        self.request = Some(request);
        self.response = Some(response);
    }

    /// The response for exactly this request, or nothing. A grant minted for
    /// a different upload is not this one's, whatever its shape.
    fn take_for(
        &self,
        request: &AttachmentDirectGrantRequest,
    ) -> Option<AttachmentDirectGrantResponse> {
        if self.request.as_ref()? != request {
            return None;
        }
        self.response.clone()
    }
}

/// What a direct-carrier attempt needs from the tab: the identity the grant is
/// bound to, the door a loopback socket would open against, and whether this
/// build can open a direct carrier at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectEnvironmentFacts {
    /// This document's tab id, as the coordinator knows it.
    pub tab_id: String,
    /// The device fingerprint the grant is bound to.
    pub device_fingerprint: String,
    /// A local worker door discovered this session, when one was.
    pub local_door: Option<LocalWorkerDoor>,
    /// Whether this build can open a direct carrier at all. False until the
    /// carrier driver reports one, which is what leaves the relay as the
    /// carrier rather than a pretended-open one.
    pub carrier_available: bool,
    /// The grant the driver minted ahead of the decision, because minting it
    /// is a network call and route selection is synchronous.
    pub pending_grant: PendingGrant,
}

impl AttachmentDirectEnvironment for DirectEnvironmentFacts {
    fn read_local_worker_door(&self) -> Option<LocalWorkerDoor> {
        self.local_door.clone()
    }

    fn peer_available(&self) -> bool {
        self.carrier_available
    }

    fn tab_id(&self) -> String {
        self.tab_id.clone()
    }

    fn device_fingerprint(&self) -> String {
        self.device_fingerprint.clone()
    }

    /// Minting a grant is a network call, which this synchronous decision
    /// cannot make, so the driver asks first and records the answer against the
    /// tuple it asked for. The loader's own fold still decides whether that
    /// answer is a grant: no grant id, no secret, or no worker epoch each leave
    /// nothing a carrier could authenticate with.
    fn mint_grant(
        &mut self,
        request: &AttachmentDirectGrantRequest,
    ) -> Option<AttachmentDirectGrantResponse> {
        self.pending_grant.take_for(request)
    }

    fn create_peer_id(&mut self) -> Option<String> {
        None
    }

    fn open_loopback_route(
        &mut self,
        _door: &LocalWorkerDoor,
        _grant: &AttachmentDirectGrant,
    ) -> RouteOpen {
        RouteOpen::Fatal("no attachment loopback carrier is open in this build".to_owned())
    }

    fn open_peer_route(&mut self, _grant: &AttachmentDirectGrant, _peer_id: &str) -> RouteOpen {
        RouteOpen::Fatal("no attachment peer carrier is open in this build".to_owned())
    }
}

/// Ask the coordinator for the grant this upload needs, or name why not.
///
/// The coordinator installs the grant on the worker before it answers, so an
/// error here is an upload that never touched a wire.
pub async fn mint_direct_grant(
    pump: &Pump,
    request: &AttachmentDirectUploadRequest,
    tab_id: &str,
) -> Option<roost_client_core::client::attachments::grant::AttachmentDirectGrantResponse> {
    let call = GrantAttachmentDirect {
        session_id: request.session_id.clone(),
        worker_fp: request.worker_fp.clone().unwrap_or_default(),
        tab_id: tab_id.to_owned(),
        upload_id: request.upload_id.clone(),
        filename: request.file_name.clone(),
        short_path: request.short_path,
        total_bytes: request.file_bytes,
    };
    match pump.rpc().call(&call).await {
        Ok(response) => Some(response),
        Err(error) => {
            tracing::warn!(
                target: "attachments",
                session = %request.session_id,
                %error,
                "direct attachment grant refused; the relay carries this upload"
            );
            None
        }
    }
}

/// The worker's durable view of one direct upload, for a receipt that a lost
/// acknowledgement needs. `None` is an answer, not a failure: the coordinator
/// holds nothing under this upload id.
pub async fn read_direct_status(
    pump: &Pump,
    session_id: &str,
    upload_id: &str,
) -> Option<AttachmentTransferStatus> {
    let call = ReadAttachmentDirectStatus {
        session_id: session_id.to_owned(),
        upload_id: upload_id.to_owned(),
    };
    match pump.rpc().call(&call).await {
        Ok(status) => status,
        Err(error) => {
            tracing::warn!(
                target: "attachments",
                %session_id,
                %upload_id,
                %error,
                "direct attachment status unavailable; the write stays unconfirmed"
            );
            None
        }
    }
}

/// Ask the worker whether it already holds these exact bytes.
///
/// Best-effort by design: a probe that fails must fall through to a normal
/// upload, because a dedup miss costs an upload and a dedup service outage
/// must not cost one too.
pub async fn probe_deduplicated(
    pump: &Pump,
    session_id: &str,
    sha256: &str,
    total_bytes: u64,
    file_name: &str,
    short_path: bool,
) -> Option<String> {
    if total_bytes == 0 || total_bytes > super::upload_plan::DEDUP_MAX_BYTES {
        return None;
    }
    let call = ProbeAttachment {
        session_id: session_id.to_owned(),
        sha256: sha256.to_owned(),
        size: total_bytes,
        filename: file_name.to_owned(),
        short_path,
    };
    match pump.rpc().call(&call).await {
        Ok(answer) if answer.hit && !answer.abs_path.is_empty() => Some(answer.abs_path),
        Ok(_) => None,
        Err(error) => {
            tracing::debug!(
                target: "attachments",
                %session_id,
                %error,
                "attachment dedup probe failed; uploading normally"
            );
            None
        }
    }
}

/// Send one relayed chunk and report the path the coordinator answered.
///
/// The path is populated only on the final chunk; every other answer is
/// deliberately the empty string rather than an error, because an
/// acknowledged non-final chunk is a success with nothing to say.
pub async fn send_relay_chunk(pump: &Pump, chunk: &RelayChunk) -> Result<String, String> {
    let call = WriteAttachmentChunk {
        upload_id: chunk.upload_id.clone(),
        session_id: chunk.session_id.clone(),
        filename: chunk.filename.clone(),
        short_path: chunk.short_path,
        data: chunk.data.clone(),
        last: chunk.last,
        seq: chunk.seq,
    };
    pump.rpc()
        .call(&call)
        .await
        .map_err(|error| error.to_string())
}

/// Run the whole-file upload over the coordinator relay, one chunk at a time.
///
/// Always sends at least one chunk, so a zero-byte file still creates a file
/// and still returns a path — a file with no chunks is a file that was never
/// created, on this route as much as on the direct one.
pub async fn relay_upload(
    pump: &Pump,
    plan: &UploadPlan,
    bytes: &[u8],
    mut on_progress: impl FnMut(u64),
) -> Result<AttachmentTransferResult, String> {
    let mut upload = RelayUpload::new(&plan.direct_request);
    while let Some(slice) = upload.next_slice() {
        let start = slice.offset as usize;
        let end = start + slice.bytes;
        let data = bytes.get(start..end).ok_or_else(|| {
            format!(
                "file changed while uploading: wanted bytes {start}..{end} of {}",
                bytes.len()
            )
        })?;
        let chunk = upload
            .frame(data.to_vec())
            .ok_or_else(|| "the upload finished before its last chunk was sent".to_owned())?;
        let abs_path = send_relay_chunk(pump, &chunk).await?;
        let settled = upload.settle(&abs_path);
        on_progress(settled);
        if let Some(outcome) = upload.outcome() {
            return Ok(outcome);
        }
    }
    Err("the upload sent every chunk and the worker never committed a file".to_owned())
}
