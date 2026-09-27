//! The short-lived, exact authority one direct upload runs under. Called by
//! `direct` before any carrier opens; a grant names the whole upload tuple and
//! the tab and device that asked for it. Ported from `attachmentDirectGrant.ts`.
//! Depends on the device fingerprint `client::auth` caches, and on nothing else:
//! the coordinator call is the host's.

use roost_proto::AttachmentTransferHello;

/// The upload a grant must name, exactly.
///
/// A struct rather than six positional parameters because four of the six are
/// silently transposable: a grant minted for `upload_b` and a grant minted for
/// a different `total_bytes` both compile at a call site that got them mixed
/// up, and both are a grant for a DIFFERENT upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentDirectGrantRequest {
    /// The worker whose door or peer the grant opens.
    pub worker_fp: String,
    /// The session the upload belongs to.
    pub session_id: String,
    /// The client-minted upload id the carrier will name on every frame.
    pub upload_id: String,
    /// The file name the worker will save under.
    pub filename: String,
    /// Whether the worker saves under a short path.
    pub short_path: bool,
    /// The whole file's size, so the worker can refuse a short upload.
    pub total_bytes: u64,
}

/// What the coordinator answered.
///
/// The three refusals are absence, not a code: no grant id, no secret, or no
/// worker epoch each leave nothing a carrier could authenticate with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttachmentDirectGrantResponse {
    pub grant_id: String,
    pub secret: String,
    pub worker_epoch: String,
    /// Whether this worker's build speaks the peer route at all.
    pub peer_supported: bool,
    /// The STUN URLs the peer route may use, verbatim from the coordinator.
    pub stun_urls: Vec<String>,
}

/// A minted grant: the request's exact tuple plus what only the coordinator
/// and this device know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentDirectGrant {
    /// The upload this grant is for. The caller keeps it to re-check the
    /// tuple before opening a carrier.
    pub request: AttachmentDirectGrantRequest,
    pub grant_id: String,
    /// The shared secret the worker verifies before it accepts a byte.
    pub secret: String,
    /// The browser tab that asked, so a grant stolen by another tab is refused.
    pub tab_id: String,
    /// The device fingerprint, so a grant is not portable between profiles.
    pub device_fingerprint: String,
    /// The worker's current epoch: a worker that restarted invalidates it.
    pub worker_epoch: String,
    pub peer_supported: bool,
    pub stun_urls: Vec<String>,
}

impl AttachmentDirectGrant {
    /// Fold a coordinator answer into a grant, or refuse it.
    ///
    /// `None` is one outcome to the caller and never a pending state: either
    /// no answer arrived at all — an unreachable coordinator, a refused device,
    /// or a grant it declined to issue — or the answer is missing a field the
    /// grant has to carry, which is refused here rather than after the bytes
    /// are on the wire.
    #[must_use]
    pub fn from_response(
        request: AttachmentDirectGrantRequest,
        tab_id: &str,
        device_fingerprint: &str,
        response: AttachmentDirectGrantResponse,
    ) -> Option<Self> {
        if response.grant_id.is_empty()
            || response.secret.is_empty()
            || response.worker_epoch.is_empty()
        {
            return None;
        }
        Some(Self {
            request,
            grant_id: response.grant_id,
            secret: response.secret,
            tab_id: tab_id.to_owned(),
            device_fingerprint: device_fingerprint.to_owned(),
            worker_epoch: response.worker_epoch,
            peer_supported: response.peer_supported,
            stun_urls: response.stun_urls,
        })
    }

    /// Whether this grant authorises exactly this upload.
    ///
    /// Every field is compared, and every minted field is checked for
    /// emptiness, because a carrier that opens on a half-populated grant fails
    /// later — after the user's bytes are on the wire — instead of here.
    #[must_use]
    pub fn admits(&self, request: &AttachmentDirectGrantRequest) -> bool {
        self.request == *request
            && !self.grant_id.is_empty()
            && !self.secret.is_empty()
            && !self.tab_id.is_empty()
            && !self.device_fingerprint.is_empty()
            && !self.worker_epoch.is_empty()
    }

    /// The authentication frame a carrier sends before any byte.
    ///
    /// `peer_id` is empty on loopback, which is how the worker tells the two
    /// routes apart; the rest of the tuple is identical either way.
    #[must_use]
    pub fn hello(&self, peer_id: &str) -> AttachmentTransferHello {
        let request = &self.request;
        AttachmentTransferHello {
            grant_id: self.grant_id.clone(),
            secret: self.secret.clone(),
            tab_id: self.tab_id.clone(),
            device_fingerprint: self.device_fingerprint.clone(),
            session_id: request.session_id.clone(),
            upload_id: request.upload_id.clone(),
            filename: request.filename.clone(),
            short_path: request.short_path,
            total_bytes: request.total_bytes,
            peer_id: peer_id.to_owned(),
            worker_epoch: self.worker_epoch.clone(),
            ..Default::default()
        }
    }
}
