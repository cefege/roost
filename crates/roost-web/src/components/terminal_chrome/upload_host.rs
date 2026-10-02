//! The browser half of the upload driver: the direct-carrier environment the
//! route chooser runs against, the coordinator calls an upload makes, and the
//! coordinator-relay chunk loop.
//!
//! The route ORDER is `client::attachments::direct`'s; this file answers what
//! the tab can reach (the discovered door, whether WebRTC exists) and performs
//! the acts the chooser asks for, on the carriers in `platform::attachments`.
//! Ports the host half of `apps/web/src/lib/attachments.ts` and the default
//! dependencies of `apps/web/src/client/attachments/attachmentDirect.ts`.

use roost_client_core::client::attachments::direct::relay::{RelayChunk, RelayUpload};
use roost_client_core::client::attachments::direct::{
    AttachmentDirectEnvironment, AttachmentDirectUploadRequest, RouteOutcome,
};
use roost_client_core::client::attachments::grant::{
    AttachmentDirectGrant, AttachmentDirectGrantRequest, AttachmentDirectGrantResponse,
};
use roost_client_core::client::attachments::transfer::AttachmentTransferResult;
use roost_client_core::client::attachments::transfer::receipt::AttachmentTransferStatus;
use roost_client_core::client::local::discovery::LocalWorkerDoor;
use roost_client_core::client::rpc::calls::attachment_direct::{
    GrantAttachmentDirect, ReadAttachmentDirectStatus,
};
use roost_client_core::client::rpc::calls::attachments::{ProbeAttachment, WriteAttachmentChunk};

use super::upload_plan::UploadPlan;
use crate::pump::Pump;

/// The identity a direct grant is bound to, read once per gesture so every
/// file of one pick is bound to the same tab and device.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectIdentity {
    /// This document's tab id, as the coordinator knows it.
    pub tab_id: String,
    /// The device fingerprint the grant is bound to.
    pub device_fingerprint: String,
}

/// What one upload's direct attempt runs against: the tab's reach, read at
/// upload time as v2 reads it, and the bytes a carrier would send.
pub struct BrowserDirectEnvironment<'upload> {
    pub pump: &'upload Pump,
    pub identity: &'upload DirectIdentity,
    pub request: &'upload AttachmentDirectUploadRequest,
    pub bytes: &'upload [u8],
    pub on_progress: &'upload dyn Fn(u64),
}

impl std::fmt::Debug for BrowserDirectEnvironment<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserDirectEnvironment")
            .field("request", self.request)
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

impl AttachmentDirectEnvironment for BrowserDirectEnvironment<'_> {
    fn read_local_worker_door(&self) -> Option<LocalWorkerDoor> {
        self.pump.local_worker_door()
    }

    /// v2: `isSecureContext && typeof RTCPeerConnection !== "undefined"`.
    fn peer_available(&self) -> bool {
        let secure = web_sys::window().is_some_and(|window| window.is_secure_context());
        secure && crate::platform::peer::is_available()
    }

    fn tab_id(&self) -> String {
        self.identity.tab_id.clone()
    }

    fn device_fingerprint(&self) -> String {
        self.identity.device_fingerprint.clone()
    }

    async fn mint_grant(
        &mut self,
        request: &AttachmentDirectGrantRequest,
    ) -> Option<AttachmentDirectGrantResponse> {
        mint_direct_grant(self.pump, request, &self.identity.tab_id).await
    }

    /// A fresh `crypto.randomUUID()` per attempt: the coordinator refuses any
    /// other peer id shape.
    fn create_peer_id(&mut self) -> Option<String> {
        super::upload_id::mint_upload_id()
    }

    async fn carry_on_loopback(
        &mut self,
        door: &LocalWorkerDoor,
        grant: &AttachmentDirectGrant,
    ) -> RouteOutcome {
        #[cfg(target_arch = "wasm32")]
        {
            use crate::platform::attachments::loopback::AttachmentLoopbackCarrier;
            match AttachmentLoopbackCarrier::open(door, grant).await {
                Ok(mut carrier) => self.send_on(&mut carrier).await,
                Err(refusal) => RouteOutcome::Refused(refusal),
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (door, grant);
            RouteOutcome::Fatal("this build has no browser socket".to_owned())
        }
    }

    async fn carry_on_peer(
        &mut self,
        grant: &AttachmentDirectGrant,
        peer_id: &str,
    ) -> RouteOutcome {
        #[cfg(target_arch = "wasm32")]
        {
            use crate::platform::attachments::peer::AttachmentPeerCarrier;
            let pump = self.pump;
            let negotiate = async |request| negotiate_attachment_peer(pump, request).await;
            match AttachmentPeerCarrier::open(grant, peer_id, negotiate).await {
                Ok(mut carrier) => self.send_on(&mut carrier).await,
                Err(refusal) => RouteOutcome::Refused(refusal),
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (grant, peer_id);
            RouteOutcome::Fatal("this build has no browser peer".to_owned())
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl BrowserDirectEnvironment<'_> {
    /// Run the chunk loop on an opened carrier and report how the route ended.
    async fn send_on<C: crate::platform::attachments::AttachmentCarrier>(
        &self,
        carrier: &mut C,
    ) -> RouteOutcome {
        use crate::platform::attachments::send_file::send_attachment_file;
        let pump = self.pump;
        let session_id = self.request.session_id.as_str();
        let coordinator_status =
            async |upload_id: &str| read_direct_status(pump, session_id, upload_id).await;
        match send_attachment_file(
            carrier,
            &self.request.upload_id,
            self.bytes,
            self.on_progress,
            coordinator_status,
        )
        .await
        {
            Ok(result) => RouteOutcome::Carried(result),
            Err(error) => RouteOutcome::Refused(error),
        }
    }
}

/// Ask the coordinator for the grant this upload needs, or name why not.
///
/// The coordinator installs the grant on the worker before it answers, so an
/// error here is an upload that never touched a wire.
async fn mint_direct_grant(
    pump: &Pump,
    request: &AttachmentDirectGrantRequest,
    tab_id: &str,
) -> Option<AttachmentDirectGrantResponse> {
    let call = GrantAttachmentDirect {
        session_id: request.session_id.clone(),
        worker_fp: request.worker_fp.clone(),
        tab_id: tab_id.to_owned(),
        upload_id: request.upload_id.clone(),
        filename: request.filename.clone(),
        short_path: request.short_path,
        total_bytes: request.total_bytes,
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

/// Put one attachment peer's offer to the coordinator, which relays it to the
/// worker and returns the worker's answer unchecked; the signaling machine
/// checks it.
#[cfg(target_arch = "wasm32")]
async fn negotiate_attachment_peer(
    pump: &Pump,
    request: roost_client_core::client::attachments::signaling::AttachmentPeerNegotiationRequest,
) -> Result<
    roost_client_core::client::attachments::signaling::AttachmentPeerNegotiationResponse,
    String,
> {
    pump.rpc()
        .call(&request)
        .await
        .map_err(|error| error.to_string())
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
