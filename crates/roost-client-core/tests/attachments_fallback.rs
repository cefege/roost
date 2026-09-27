impl FakeEnvironment {
    fn new() -> Self {
        Self {
            door: None,
            peer_available: true,
            tab_id: "tab-a".to_owned(),
            device_fingerprint: "device-a".to_owned(),
            mint: Ok(AttachmentDirectGrantResponse {
                grant_id: "grant-a".to_owned(),
                secret: "secret-a".to_owned(),
                worker_epoch: "epoch-a".to_owned(),
                peer_supported: true,
                stun_urls: Vec::new(),
            }),
            peer_id: Some("peer-a".to_owned()),
            loopback: RouteOpen::Opened,
            peer: RouteOpen::Opened,
            calls: Vec::new(),
            minted: Vec::new(),
        }
    }

    fn with_door(worker_fingerprint: &str) -> Self {
        Self {
            door: Some(LocalWorkerDoor {
                origin: "http://127.0.0.1:4104".to_owned(),
                worker_fingerprint: worker_fingerprint.to_owned(),
            }),
            ..Self::new()
        }
    }

    fn refused(reason: &str, sent_chunk: bool) -> RouteOpen {
        RouteOpen::Refused(AttachmentTransferCarrierError::refused(reason, sent_chunk))
    }
}

impl AttachmentDirectEnvironment for FakeEnvironment {
    fn read_local_worker_door(&self) -> Option<LocalWorkerDoor> {
        self.door.clone()
    }

    fn peer_available(&self) -> bool {
        self.peer_available
    }

    fn tab_id(&self) -> String {
        self.tab_id.clone()
    }

    fn device_fingerprint(&self) -> String {
        self.device_fingerprint.clone()
    }

    fn mint_grant(
        &mut self,
        request: &AttachmentDirectGrantRequest,
    ) -> Result<AttachmentDirectGrantResponse, ()> {
        self.calls.push("mint".to_owned());
        self.minted.push(request.clone());
        self.mint.clone()
    }

    fn create_peer_id(&mut self) -> Option<String> {
        self.calls.push("peer-id".to_owned());
        self.peer_id.clone()
    }

    fn open_loopback_route(
        &mut self,
        _door: &LocalWorkerDoor,
        _grant: &AttachmentDirectGrant,
    ) -> RouteOpen {
        self.calls.push("loopback".to_owned());
        self.loopback.clone()
    }

    fn open_peer_route(&mut self, _grant: &AttachmentDirectGrant, _peer_id: &str) -> RouteOpen {
        self.calls.push("peer".to_owned());
        self.peer.clone()
    }
}

/// The upload these tests are about: two bytes, on a named worker.
fn request() -> AttachmentDirectUploadRequest {
    AttachmentDirectUploadRequest {
        worker_fp: Some("worker-a".to_owned()),
        session_id: "session-a".to_owned(),
        upload_id: "upload-a".to_owned(),
        file_name: "direct.bin".to_owned(),
        file_bytes: 2,
        short_path: false,
    }
}

/// The route an `Opened` attempt chose.
fn opened_route(attempt: &DirectAttempt) -> DirectRoute {
    match attempt {
        DirectAttempt::Opened { route, .. } => *route,
        other => panic!("expected an opened carrier, got {other:?}"),
    }
}

#[test]
fn uses_a_matching_loopback_worker_door_before_webrtc() {
    let mut environment = FakeEnvironment::with_door("worker-a");
    let attempt = upload_attachment_direct(&request(), &mut environment);

    assert_eq!(opened_route(&attempt), DirectRoute::Loopback);
    assert_eq!(
        environment.calls,
        vec!["mint", "loopback"],
        "the peer is never opened behind a matching door"
    );
    assert_eq!(
        environment.minted,
        vec![AttachmentDirectGrantRequest {
            worker_fp: "worker-a".to_owned(),
            session_id: "session-a".to_owned(),
            upload_id: "upload-a".to_owned(),
            filename: "direct.bin".to_owned(),
            short_path: false,
            total_bytes: 2,
        }],
        "the grant names the whole upload, and nothing else"
    );
}

#[test]
fn fences_a_mismatched_door_and_advances_a_pre_send_loopback_failure_to_webrtc() {
    // A door for another worker is fenced off, so the peer is the only route.
    let mut fenced = FakeEnvironment::with_door("other-worker");
    let attempt = upload_attachment_direct(&request(), &mut fenced);
    assert_eq!(opened_route(&attempt), DirectRoute::Peer);
    assert_eq!(fenced.calls, vec!["mint", "peer"]);

    // A door that matches but refuses before any chunk is also fenced off, and
    // the peer follows it.
    let mut refused = FakeEnvironment::with_door("worker-a");
    refused.loopback = FakeEnvironment::refused("loopback unavailable", false);
    let attempt = upload_attachment_direct(&request(), &mut refused);
    assert_eq!(opened_route(&attempt), DirectRoute::Peer);
    assert_eq!(
        refused.calls,
        vec!["mint", "loopback", "peer-id", "peer"],
        "a refusal before the first chunk is the one that may fall back"
    );
}

#[test]
fn never_switches_to_webrtc_after_a_loopback_chunk_was_sent() {
    let mut environment = FakeEnvironment::with_door("worker-a");
    environment.loopback = FakeEnvironment::refused("loopback failed after send", true);

    let attempt = upload_attachment_direct(&request(), &mut environment);

    let DirectAttempt::FailedWithBytes(failure) = attempt else {
        panic!("bytes left the browser, so this is not an unavailable route");
    };
    assert_eq!(failure.reason, "loopback failed after send");
    assert!(failure.sent_chunk);
    assert_eq!(
        environment.calls,
        vec!["mint", "loopback"],
        "the peer must never see this upload, or the bytes are written twice"
    );
}

#[test]
fn does_not_mint_a_direct_grant_when_neither_local_carrier_is_possible() {
    let mut environment = FakeEnvironment {
        peer_available: false,
        ..FakeEnvironment::new()
    };
    let attempt = upload_attachment_direct(&request(), &mut environment);

    assert_eq!(
        attempt,
        DirectAttempt::Unavailable(DirectUnavailableReason::NoLocalCarrier)
    );
    assert!(
        environment.minted.is_empty(),
        "no grant is minted for an upload that cannot leave directly: a grant is a coordinator-side record"
    );
}

// ------------------------------------------------- one test per named reason

#[test]
fn a_session_with_no_worker_names_no_worker_fingerprint() {
    let mut environment = FakeEnvironment::new();
    let mut upload = request();
    upload.worker_fp = None;

    assert_eq!(
        upload_attachment_direct(&upload, &mut environment),
        DirectAttempt::Unavailable(DirectUnavailableReason::NoWorkerFingerprint)
    );
    assert!(environment.minted.is_empty());
    assert!(environment.calls.is_empty(), "nothing is asked of anybody");
}

#[test]
fn a_size_no_peer_could_read_exactly_names_an_unrepresentable_file_size() {
    let mut environment = FakeEnvironment::with_door("worker-a");
    let mut upload = request();
    upload.file_bytes = MAX_SAFE_TOTAL_BYTES + 1;

    assert_eq!(
        upload_attachment_direct(&upload, &mut environment),
        DirectAttempt::Unavailable(DirectUnavailableReason::UnrepresentableFileSize)
    );
    assert!(environment.minted.is_empty());
}

#[test]
fn a_refused_or_unreachable_coordinator_names_a_refused_grant() {
    let mut environment = FakeEnvironment::with_door("worker-a");
    environment.mint = Err(());

    assert_eq!(
        upload_attachment_direct(&request(), &mut environment),
        DirectAttempt::Unavailable(DirectUnavailableReason::GrantRefused)
    );
    assert_eq!(
        environment.calls,
        vec!["mint"],
        "no route is opened without a grant"
    );
}

#[test]
fn a_grant_with_no_tab_or_device_names_a_grant_mismatch() {
    let mut environment = FakeEnvironment::with_door("worker-a");
    environment.tab_id = String::new();

    assert_eq!(
        upload_attachment_direct(&request(), &mut environment),
        DirectAttempt::Unavailable(DirectUnavailableReason::GrantMismatch),
        "a grant bound to no tab authorises nothing"
    );
    assert_eq!(environment.calls, vec!["mint"]);
}

#[test]
fn a_worker_without_the_peer_route_names_an_unsupported_peer() {
    let mut environment = FakeEnvironment::with_door("worker-a");
    environment.loopback = FakeEnvironment::refused("loopback unavailable", false);
    environment.mint = Ok(AttachmentDirectGrantResponse {
        grant_id: "grant-a".to_owned(),
        secret: "secret-a".to_owned(),
        worker_epoch: "epoch-a".to_owned(),
        peer_supported: false,
        stun_urls: Vec::new(),
    });

    assert_eq!(
        upload_attachment_direct(&request(), &mut environment),
        DirectAttempt::Unavailable(DirectUnavailableReason::PeerUnsupported)
    );
    assert_eq!(environment.calls, vec!["mint", "loopback"]);
}

#[test]
fn an_unmintable_peer_id_names_an_unavailable_peer_id() {
    let mut environment = FakeEnvironment::with_door("worker-a");
    environment.loopback = FakeEnvironment::refused("loopback unavailable", false);
    environment.peer_id = None;

    assert_eq!(
        upload_attachment_direct(&request(), &mut environment),
        DirectAttempt::Unavailable(DirectUnavailableReason::PeerIdUnavailable)
    );
    assert_eq!(environment.calls, vec!["mint", "loopback", "peer-id"]);
}

#[test]
fn two_refused_routes_name_a_refused_peer() {
    let mut environment = FakeEnvironment::with_door("worker-a");
    environment.loopback = FakeEnvironment::refused("loopback unavailable", false);
    environment.peer = FakeEnvironment::refused("peer negotiation failed", false);

    assert_eq!(
        upload_attachment_direct(&request(), &mut environment),
        DirectAttempt::Unavailable(DirectUnavailableReason::PeerRefused),
        "neither route put a byte anywhere, so the relay is still an untouched carrier"
    );
    assert_eq!(
        environment.calls,
        vec!["mint", "loopback", "peer-id", "peer"]
    );
}

#[test]
fn a_fatal_route_failure_is_not_a_fallback_signal() {
    let mut environment = FakeEnvironment::with_door("worker-a");
    environment.loopback = RouteOpen::Fatal("WebSocket is unavailable".to_owned());

    assert_eq!(
        upload_attachment_direct(&request(), &mut environment),
        DirectAttempt::Failed {
            route: DirectRoute::Loopback,
            reason: "WebSocket is unavailable".to_owned(),
        },
        "a thrown error is a failure, not an unavailable route"
    );
    assert_eq!(environment.calls, vec!["mint", "loopback"]);
}
