//! Which direct carrier one upload may use: a matching local door, then the
//! peer, then coordinator relay. Called by the upload driver; it decides the
//! route and names the reason there is none, and never sends anything. Ported
//! from `attachmentDirect.ts`. Depends on `grant`, `transfer` and `loopback`.
//! The refusals it returns are the whole direct-then-peer fallback.

pub mod loopback;
pub mod relay;

use super::grant::{
    AttachmentDirectGrant, AttachmentDirectGrantRequest, AttachmentDirectGrantResponse,
};
use super::transfer::{AttachmentTransferCarrierError, DirectUpload, MAX_SAFE_TOTAL_BYTES};

/// A local worker door: the origin its loopback socket is opened against, and
/// the worker it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorkerDoor {
    pub origin: String,
    pub worker_fingerprint: String,
}

impl LocalWorkerDoor {
    /// The `ws:`/`wss:` URL this door's loopback socket is opened against.
    ///
    /// An `https` door becomes `wss`, because a door served over TLS is not
    /// reachable over a plaintext socket, and the bytes on it are the user's
    /// file.
    #[must_use]
    pub fn loopback_url(&self) -> String {
        let (scheme, rest) = self
            .origin
            .split_once("://")
            .unwrap_or(("http", self.origin.as_str()));
        let socket_scheme = match scheme {
            "https" | "wss" => "wss",
            _ => "ws",
        };
        let path = loopback::LOOPBACK_PATH;
        format!("{socket_scheme}://{rest}{path}")
    }
}

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

/// How a route open ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteOpen {
    /// The carrier is open and the upload may begin on it.
    Opened,
    /// The route was not usable and no upload bytes left, so the next route
    /// may be tried.
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
    /// A carrier is open. `upload` is the chunk loop that settles on it.
    Opened {
        route: DirectRoute,
        upload: Box<DirectUpload>,
    },
    /// The route failed with upload bytes on the wire. Only that route's own
    /// status control may settle it; another route would write the bytes twice.
    FailedWithBytes(AttachmentTransferCarrierError),
    /// The route failed in a way that is not a carrier refusal. The upload
    /// fails here and does NOT fall back to the relay.
    Failed {
        route: DirectRoute,
        reason: String,
    },
}

/// Everything the loader needs from a host, and nothing it can decide itself.
///
/// The two `open` methods return a [`RouteOpen`] rather than a carrier object:
/// the loader's only question is whether a route opened, and the host keeps
/// whatever socket or peer connection it just opened.
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
    fn mint_grant(
        &mut self,
        request: &AttachmentDirectGrantRequest,
    ) -> Result<AttachmentDirectGrantResponse, ()>;

    /// Mint the peer id this attempt will be known by.
    fn create_peer_id(&mut self) -> Option<String>;

    /// Open the loopback carrier for this door and grant.
    fn open_loopback_route(
        &mut self,
        door: &LocalWorkerDoor,
        grant: &AttachmentDirectGrant,
    ) -> RouteOpen;

    /// Open the peer carrier for this grant and peer id.
    fn open_peer_route(&mut self, grant: &AttachmentDirectGrant, peer_id: &str) -> RouteOpen;
}

/// Choose the carrier for one upload, or name the reason there is none.
pub fn upload_attachment_direct(
    request: &AttachmentDirectUploadRequest,
    environment: &mut dyn AttachmentDirectEnvironment,
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
    let response = environment.mint_grant(&grant_request);
    let tab_id = environment.tab_id();
    let fingerprint = environment.device_fingerprint();
    let Some(grant) =
        AttachmentDirectGrant::from_response(grant_request.clone(), &tab_id, &fingerprint, response)
    else {
        return DirectAttempt::Unavailable(DirectUnavailableReason::GrantRefused);
    };
    if !grant.admits(&grant_request) {
        return DirectAttempt::Unavailable(DirectUnavailableReason::GrantMismatch);
    }
    if let Some(door) = matching_door {
        match environment.open_loopback_route(&door, &grant) {
            RouteOpen::Opened => return opened_attempt(DirectRoute::Loopback, request),
            RouteOpen::Fatal(reason) => {
                return DirectAttempt::Failed {
                    route: DirectRoute::Loopback,
                    reason,
                };
            }
            // No bytes left, so the peer route is still an untouched carrier.
            RouteOpen::Refused(_) => {}
        }
    }
    if !grant.peer_supported || !peer_available {
        return DirectAttempt::Unavailable(DirectUnavailableReason::PeerUnsupported);
    }
    let Some(peer_id) = environment.create_peer_id() else {
        return DirectAttempt::Unavailable(DirectUnavailableReason::PeerIdUnavailable);
    };
    match environment.open_peer_route(&grant, &peer_id) {
        RouteOpen::Opened => opened_attempt(DirectRoute::Peer, request),
        RouteOpen::Fatal(reason) => DirectAttempt::Failed {
            route: DirectRoute::Peer,
            reason,
        },
        // Both routes were refused before any byte left, so the relay is still
        // an untouched carrier rather than a second copy of the file.
        RouteOpen::Refused(_) => DirectAttempt::Unavailable(DirectUnavailableReason::PeerRefused),
    }
}

/// The one outcome that means a carrier is open: a fresh chunk loop for it.
fn opened_attempt(route: DirectRoute, request: &AttachmentDirectUploadRequest) -> DirectAttempt {
    DirectAttempt::Opened {
        route,
        upload: Box::new(DirectUpload::new(&request.upload_id, request.file_bytes)),
    }
}
