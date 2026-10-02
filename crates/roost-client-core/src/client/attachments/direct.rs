//! Which direct carrier one upload may use: a matching local door, then the
//! peer, then coordinator relay. Called by the upload driver; it decides the
//! route order and names the reason there is none, and the host performs every
//! open and send it asks for. Ported from `attachmentDirect.ts`. Depends on
//! `grant`, `transfer` and `loopback`. The refusals it returns are the whole
//! direct-then-peer fallback.

pub mod loopback;
pub mod relay;

use super::grant::{
    AttachmentDirectGrant, AttachmentDirectGrantRequest, AttachmentDirectGrantResponse,
};
use super::transfer::{
    AttachmentTransferCarrierError, AttachmentTransferResult, MAX_SAFE_TOTAL_BYTES,
};
use crate::client::local::discovery::LocalWorkerDoor;

/// One browser file, and the session it is going to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentDirectUploadRequest {
    /// The session's worker. `None` means the session has no worker at all, so
    /// there is no door to match and no peer to negotiate with.
    pub worker_fp: Option<String>,
    pub session_id: String,
    /// The client-minted upload id, which the grant and every frame name.
    pub upload_id: String,
    pub file_name: String,
    pub file_bytes: u64,
    pub short_path: bool,
}

impl AttachmentDirectUploadRequest {
    /// The exact tuple a grant for this upload must name.
    #[must_use]
    pub fn grant_request(&self, worker_fp: &str) -> AttachmentDirectGrantRequest {
        AttachmentDirectGrantRequest {
            worker_fp: worker_fp.to_owned(),
            session_id: self.session_id.clone(),
            upload_id: self.upload_id.clone(),
            filename: self.file_name.clone(),
            short_path: self.short_path,
            total_bytes: self.file_bytes,
        }
    }
}

/// Which direct route a grant opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectRoute {
    /// The fingerprint-matching local worker door.
    Loopback,
    /// The negotiated WebRTC peer.
    Peer,
}

impl DirectRoute {
    /// The wire spelling, for a log line and a card.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Loopback => "loopback",
            Self::Peer => "peer",
        }
    }
}

/// How one route's attempt ended. A route is tried by opening it AND sending
/// the whole file on it, because v2's fallback boundary is the first chunk
/// leaving, not the carrier opening (`attachmentDirect.ts:100-120`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteOutcome {
    /// The route carried every chunk and the worker committed the file.
    Carried(AttachmentTransferResult),
    /// The route failed. `sent_chunk` on the error says whether any upload
    /// byte left on it, which is what decides whether another route may run.
    Refused(AttachmentTransferCarrierError),
    /// The route failed in a way that is not a carrier refusal — a thrown
    /// error, a socket constructor that does not exist. Not a fallback signal:
    /// v2 propagated these rather than treating them as an unavailable route.
    Fatal(String),
}

/// Why no direct route carried this upload. Every one of these leaves
/// coordinator relay as the carrier, and none of them has put a byte anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectUnavailableReason {
    /// The session names no worker, so there is no door to match and no peer.
    NoWorkerFingerprint,
    /// The file's size is larger than a peer could read exactly, so a grant
    /// naming it would be refused at the far end instead of here.
    UnrepresentableFileSize,
    /// No local door matches this worker and the browser has no peer at all.
    NoLocalCarrier,
    /// The coordinator declined to issue a grant, or could not be reached.
    GrantRefused,
    /// A grant came back that does not authorise this upload, so it is not
    /// this one's.
    GrantMismatch,
    /// Loopback was refused before its first chunk and the worker does not
    /// speak the peer route, or this browser cannot build one.
    PeerUnsupported,
    /// The peer id this attempt needs could not be minted.
    PeerIdUnavailable,
    /// Both routes were tried and both were refused before their first chunk.
    PeerRefused,
}

/// What the loader decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectAttempt {
    /// No direct route carried this upload: fall back to coordinator relay.
    Unavailable(DirectUnavailableReason),
    /// A direct route carried the upload and the worker committed it.
    Carried {
        route: DirectRoute,
        result: AttachmentTransferResult,
    },
    /// The route failed with upload bytes on the wire. Only that route's own
    /// status control may settle it; another route would write the bytes twice.
    FailedWithBytes(AttachmentTransferCarrierError),
    /// The route failed in a way that is not a carrier refusal. The upload
    /// fails here and does NOT fall back to the relay.
    Failed { route: DirectRoute, reason: String },
}

/// Everything the loader needs from a host, and nothing it can decide itself.
///
/// The network acts are futures because a browser answers every one of them
/// on a later task; the loader awaits each before it decides the next step, so
/// the route ORDER stays here and only the acts live in the host.
pub trait AttachmentDirectEnvironment {
    /// The local worker door, when one has been discovered this session.
    fn read_local_worker_door(&self) -> Option<LocalWorkerDoor>;

    /// Whether this browser can build a peer connection at all.
    fn peer_available(&self) -> bool;

    /// This document's tab id, which the grant is bound to.
    fn tab_id(&self) -> String;

    /// The device fingerprint the grant is bound to.
    fn device_fingerprint(&self) -> String;

    /// Ask the coordinator for the exact grant this upload needs.
    ///
    /// `None` is every way there is no answer to fold, and none of them is a
    /// pending state — the same shape as `create_peer_id` below, which answers
    /// the same way when there is no id to mint.
    fn mint_grant(
        &mut self,
        request: &AttachmentDirectGrantRequest,
    ) -> impl Future<Output = Option<AttachmentDirectGrantResponse>>;

    /// Mint the peer id this attempt will be known by.
    fn create_peer_id(&mut self) -> Option<String>;

    /// Open the loopback carrier for this door and grant, send the file on it,
    /// and close it.
    fn carry_on_loopback(
        &mut self,
        door: &LocalWorkerDoor,
        grant: &AttachmentDirectGrant,
    ) -> impl Future<Output = RouteOutcome>;

    /// Open the peer carrier for this grant and peer id, send the file on it,
    /// and close it.
    fn carry_on_peer(
        &mut self,
        grant: &AttachmentDirectGrant,
        peer_id: &str,
    ) -> impl Future<Output = RouteOutcome>;
}

/// Choose the carrier for one upload and run it, or name the reason there is
/// none.
pub async fn upload_attachment_direct<E: AttachmentDirectEnvironment>(
    request: &AttachmentDirectUploadRequest,
    environment: &mut E,
) -> DirectAttempt {
    let Some(worker_fp) = request.worker_fp.clone() else {
        return DirectAttempt::Unavailable(DirectUnavailableReason::NoWorkerFingerprint);
    };
    if request.file_bytes > MAX_SAFE_TOTAL_BYTES {
        return DirectAttempt::Unavailable(DirectUnavailableReason::UnrepresentableFileSize);
    }
    let door = environment.read_local_worker_door();
    let peer_available = environment.peer_available();
    let matching_door = door.filter(|door| door.worker_fingerprint == worker_fp);
    if matching_door.is_none() && !peer_available {
        return DirectAttempt::Unavailable(DirectUnavailableReason::NoLocalCarrier);
    }
    let grant_request = request.grant_request(&worker_fp);
    let tab_id = environment.tab_id();
    let fingerprint = environment.device_fingerprint();
    let Some(grant) = environment
        .mint_grant(&grant_request)
        .await
        .and_then(|response| {
            AttachmentDirectGrant::from_response(
                grant_request.clone(),
                &tab_id,
                &fingerprint,
                response,
            )
        })
    else {
        return DirectAttempt::Unavailable(DirectUnavailableReason::GrantRefused);
    };
    if !grant.admits(&grant_request) {
        return DirectAttempt::Unavailable(DirectUnavailableReason::GrantMismatch);
    }
    if let Some(door) = matching_door {
        match environment.carry_on_loopback(&door, &grant).await {
            RouteOutcome::Carried(result) => {
                return DirectAttempt::Carried {
                    route: DirectRoute::Loopback,
                    result,
                };
            }
            RouteOutcome::Fatal(reason) => {
                return DirectAttempt::Failed {
                    route: DirectRoute::Loopback,
                    reason,
                };
            }
            // A refusal that CROSSED the boundary is not a clean fallback: this
            // route put bytes on the wire, and only its own status control may
            // settle them. Falling through to the peer would write them twice.
            // v2's gate is the same check (`attachmentDirect.ts:117`:
            // `!error.sentChunk && !connection?.sentChunk`).
            RouteOutcome::Refused(error) if error.sent_chunk => {
                return DirectAttempt::FailedWithBytes(error);
            }
            // Nothing left, so the peer route is still an untouched carrier.
            RouteOutcome::Refused(_) => {}
        }
    }
    if !grant.peer_supported || !peer_available {
        return DirectAttempt::Unavailable(DirectUnavailableReason::PeerUnsupported);
    }
    let Some(peer_id) = environment.create_peer_id() else {
        return DirectAttempt::Unavailable(DirectUnavailableReason::PeerIdUnavailable);
    };
    match environment.carry_on_peer(&grant, &peer_id).await {
        RouteOutcome::Carried(result) => DirectAttempt::Carried {
            route: DirectRoute::Peer,
            result,
        },
        RouteOutcome::Fatal(reason) => DirectAttempt::Failed {
            route: DirectRoute::Peer,
            reason,
        },
        // The bytes are already on a wire only this route's status control can
        // settle, so the relay is not a fallback for them.
        RouteOutcome::Refused(error) if error.sent_chunk => DirectAttempt::FailedWithBytes(error),
        // Nothing left on either route, so the relay is still untouched.
        RouteOutcome::Refused(_) => {
            DirectAttempt::Unavailable(DirectUnavailableReason::PeerRefused)
        }
    }
}
