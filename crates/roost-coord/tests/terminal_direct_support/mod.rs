//! Shared fixtures for the direct-terminal tests: exact worker generations in
//! a per-test registry whose sent frames are captured, grant requests, a fake
//! grant port for signaling, and a valid bounded SDP. No socket, no global.
//! Ports `apps/coord/tests/terminal/direct/terminal-peer-negotiation-test-fixture.ts`
//! and the fake-worker helpers of `terminal-grant-owner.test.ts`.

// Compiled into every `terminal_direct_*.rs` binary; each uses a subset.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::collections::{BTreeSet, HashMap};
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};

use roost_coord::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use roost_coord::terminal_direct::grant_state::{
    InvalidationListener, TerminalGrantAuthorization, TerminalGrantInvalidation,
    TerminalGrantLeaseSnapshot, TerminalGrantRequest,
};
use roost_coord::terminal_direct::peer_negotiations::{
    TerminalPeerNegotiations, TerminalPeerNegotiationsOptions,
};
use roost_coord::terminal_direct::peer_state::{
    TerminalGrantSessionAuthorizer, TerminalPeerCaller, TerminalPeerGrantPort, TerminalPeerSettings,
};
use roost_proto::{
    DLocalTerminalGrant, DLocalTerminalPeerOffer, SessionsNegotiateLocalTerminalPeerRequest,
    WLocalTerminalPeerAnswer,
};
use roost_protocol::versioning::CAPABILITY_TERMINAL_PEER_WEBRTC_V1;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

pub const WORKER_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const WORKER_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const DEVICE_FP: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
pub const OWNER_KEY: &str =
    "account-device:test-account:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
pub const TAB_ID: &str = "terminal-tab";
pub const SESSION_A: &str = "00000000-0000-4000-8000-000000000001";
pub const SESSION_B: &str = "00000000-0000-4000-8000-000000000002";
pub const GRANT_ID: &str = "terminal-peer-test-grant";
pub const PEER_ID: &str = "00000000-0000-4000-8000-000000000010";

/// One exact worker generation and every frame sent to it.
#[derive(Clone)]
pub struct TestWorker {
    pub handle: Arc<WorkerHandle>,
    pub sent: Arc<Mutex<Vec<CoordWorkerDownstream>>>,
}

impl TestWorker {
    /// Every frame sent so far.
    pub fn frames(&self) -> Vec<CoordWorkerDownstream> {
        self.sent.lock().unwrap().clone()
    }

    /// Every grant install sent so far.
    pub fn grants(&self) -> Vec<DLocalTerminalGrant> {
        self.frames()
            .into_iter()
            .filter_map(|frame| match frame {
                CoordWorkerDownstream::LocalTerminalGrant(grant) => Some(grant),
                _ => None,
            })
            .collect()
    }

    /// The last offer sent.
    pub fn last_offer(&self) -> DLocalTerminalPeerOffer {
        self.frames()
            .into_iter()
            .rev()
            .find_map(|frame| match frame {
                CoordWorkerDownstream::LocalTerminalPeerOffer(offer) => Some(offer),
                _ => None,
            })
            .expect("a terminal peer offer")
    }

    /// The kind of the last frame sent, if any.
    pub fn last_kind(&self) -> Option<&'static str> {
        self.sent
            .lock()
            .unwrap()
            .last()
            .map(CoordWorkerDownstream::kind)
    }
}

/// Register a ready generation for `worker_fp`, replacing (and fencing) any
/// generation it had before.
pub fn install_worker(
    registry: &WorkerRegistry,
    worker_fp: &str,
    process_epoch: Option<&str>,
    capabilities: &[&str],
) -> TestWorker {
    let sent: Arc<Mutex<Vec<CoordWorkerDownstream>>> = Arc::default();
    let sink = Arc::clone(&sent);
    // The capture buffer's address is unique while the generation lives.
    let generation = format!(
        "{}-{}-{:p}",
        &worker_fp[..4],
        process_epoch.unwrap_or("legacy"),
        Arc::as_ptr(&sent)
    );
    let handle = Arc::new(WorkerHandle::new(
        fp(worker_fp),
        process_epoch.map(str::to_owned),
        generation,
        capabilities
            .iter()
            .map(|capability| (*capability).to_owned())
            .collect::<BTreeSet<_>>(),
        Arc::new(move |frame: CoordWorkerDownstream| {
            sink.lock().unwrap().push(frame);
            1_i64
        }),
    ));
    registry.insert(Arc::clone(&handle));
    handle.mark_ready();
    TestWorker { handle, sent }
}

pub fn fp(value: &str) -> WorkerFp {
    WorkerFp::try_from(value).expect("a worker fingerprint")
}

/// An authorization that always passes.
pub fn allow_all() -> TerminalGrantAuthorization {
    Arc::new(|_| Box::pin(async { Ok(()) }))
}

/// One browser's demand on `worker_fp`.
pub fn grant_request(
    worker_fp: &str,
    session_ids: &[&str],
    authorize: TerminalGrantAuthorization,
) -> TerminalGrantRequest {
    TerminalGrantRequest {
        owner_key: OWNER_KEY.to_owned(),
        device_fingerprint: DEVICE_FP.to_owned(),
        tab_id: TAB_ID.to_owned(),
        worker_fp: worker_fp.to_owned(),
        session_ids: session_ids.iter().map(|id| (*id).to_owned()).collect(),
        authorize,
    }
}

/// Let spawned owner tasks run until `ready` holds.
pub async fn settle_until(mut ready: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if ready() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("the owner never reached the expected state");
}

/// Let spawned owner tasks run for a while without a condition.
pub async fn yield_many() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

/// A valid, bounded data-channel SDP; fixture input only, never asserted on.
pub fn valid_sdp() -> String {
    let fingerprint: Vec<String> = (0..32).map(|index| format!("{index:02x}")).collect();
    [
        "v=0".to_owned(),
        "o=- 1 2 IN IP4 127.0.0.1".to_owned(),
        "s=-".to_owned(),
        "t=0 0".to_owned(),
        "m=application 9 UDP/DTLS/SCTP webrtc-datachannel".to_owned(),
        "a=setup:actpass".to_owned(),
        format!("a=fingerprint:sha-256 {}", fingerprint.join(":")),
        "a=ice-ufrag:peer-offer".to_owned(),
        format!("a=ice-pwd:{}", "p".repeat(22)),
        "a=max-message-size:16384".to_owned(),
        "a=candidate:host 1 udp 2122260223 192.0.2.8 5000 typ host".to_owned(),
        String::new(),
    ]
    .join("\r\n")
}

/// The fake lease port v2's fixture calls `TestTerminalGrants`.
#[derive(Default)]
pub struct TestTerminalGrants {
    leases: Mutex<HashMap<(String, String, String), TerminalGrantLeaseSnapshot>>,
    listeners: Mutex<Vec<(u64, InvalidationListener)>>,
}

impl TestTerminalGrants {
    pub fn install(&self, lease: TerminalGrantLeaseSnapshot) {
        let key = (
            lease.owner_key.clone(),
            lease.tab_id.clone(),
            lease.worker_fp.clone(),
        );
        self.leases.lock().unwrap().insert(key, lease);
    }

    pub fn invalidate(&self, invalidation: &TerminalGrantInvalidation) {
        let listeners: Vec<InvalidationListener> = self
            .listeners
            .lock()
            .unwrap()
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect();
        for listener in listeners {
            listener(invalidation);
        }
    }
}

impl TerminalPeerGrantPort for TestTerminalGrants {
    fn owned_grant(
        &self,
        owner_key: &str,
        tab_id: &str,
        worker_fp: &str,
        grant_id: &str,
    ) -> Option<TerminalGrantLeaseSnapshot> {
        let key = (
            owner_key.to_owned(),
            tab_id.to_owned(),
            worker_fp.to_owned(),
        );
        self.leases
            .lock()
            .unwrap()
            .get(&key)
            .filter(|lease| lease.grant_id == grant_id)
            .cloned()
    }

    fn subscribe_invalidation(&self, listener: InvalidationListener) -> Option<u64> {
        let mut listeners = self.listeners.lock().unwrap();
        let id = listeners.len() as u64 + 1;
        listeners.push((id, listener));
        Some(id)
    }

    fn unsubscribe_invalidation(&self, id: u64) {
        self.listeners
            .lock()
            .unwrap()
            .retain(|(listener_id, _)| *listener_id != id);
    }
}

/// The browser v2's fixture calls `TEST_CALLER`.
pub fn test_caller() -> TerminalPeerCaller {
    caller_for(&"d".repeat(64))
}

/// A browser on its own device fingerprint.
pub fn caller_for(device_fingerprint: &str) -> TerminalPeerCaller {
    TerminalPeerCaller {
        owner_key: format!("account-device:test-account:{device_fingerprint}"),
        device_fingerprint: device_fingerprint.to_owned(),
    }
}

/// A lease for `caller` on `worker`'s exact generation.
pub fn install_lease(
    grants: &TestTerminalGrants,
    worker: &Arc<WorkerHandle>,
    caller: &TerminalPeerCaller,
    tab_id: &str,
) -> TerminalGrantLeaseSnapshot {
    let lease = TerminalGrantLeaseSnapshot {
        grant_id: GRANT_ID.to_owned(),
        owner_key: caller.owner_key.clone(),
        device_fingerprint: caller.device_fingerprint.clone(),
        tab_id: tab_id.to_owned(),
        worker_fp: worker.worker_fp.as_str().to_owned(),
        worker_epoch: worker.process_epoch.clone(),
        session_ids: vec![SESSION_A.to_owned()],
        expires_at_ms: i64::MAX,
        worker_handle: Arc::clone(worker),
    };
    grants.install(lease.clone());
    lease
}

/// What a negotiation owner under test is built with.
pub struct PeerOptions {
    pub peer_enabled: bool,
    pub answer_timeout_ms: u64,
    pub authorize: TerminalGrantSessionAuthorizer,
}

impl Default for PeerOptions {
    fn default() -> Self {
        Self {
            peer_enabled: true,
            answer_timeout_ms: 8_000,
            authorize: Arc::new(|_, _| Box::pin(async { Ok(()) })),
        }
    }
}

/// A negotiation owner over the fake grant port.
pub fn negotiations(
    registry: &Arc<WorkerRegistry>,
    grants: &Arc<TestTerminalGrants>,
    options: PeerOptions,
) -> Arc<TerminalPeerNegotiations> {
    TerminalPeerNegotiations::new(TerminalPeerNegotiationsOptions {
        workers: Arc::clone(registry),
        grants: Arc::clone(grants) as Arc<dyn TerminalPeerGrantPort>,
        settings: TerminalPeerSettings {
            enabled: options.peer_enabled,
            stun_urls: Vec::new(),
        },
        authorize_sessions: options.authorize,
        answer_timeout_ms: NonZeroU64::new(options.answer_timeout_ms).expect("a timeout"),
    })
}

/// A peer-capable ready worker.
pub fn peer_worker(registry: &WorkerRegistry, worker_fp: &str, epoch: &str) -> TestWorker {
    install_worker(
        registry,
        worker_fp,
        Some(epoch),
        &[CAPABILITY_TERMINAL_PEER_WEBRTC_V1],
    )
}

/// A negotiation request for `worker`'s generation.
pub fn peer_request(
    worker: &WorkerHandle,
    tab_id: &str,
    peer_id: &str,
) -> SessionsNegotiateLocalTerminalPeerRequest {
    SessionsNegotiateLocalTerminalPeerRequest {
        worker_fp: worker.worker_fp.as_str().to_owned(),
        grant_id: GRANT_ID.to_owned(),
        tab_id: tab_id.to_owned(),
        peer_id: peer_id.to_owned(),
        offer_sdp: valid_sdp(),
        worker_epoch: worker.process_epoch.clone().unwrap_or_default(),
        ..Default::default()
    }
}

/// The worker's answer to `offer`, from `worker`'s generation.
pub fn peer_answer(
    offer: &DLocalTerminalPeerOffer,
    worker: &WorkerHandle,
) -> WLocalTerminalPeerAnswer {
    WLocalTerminalPeerAnswer {
        request_id: offer.request_id.clone(),
        connection_generation: worker.connection_generation.clone(),
        worker_epoch: worker.process_epoch.clone().unwrap_or_default(),
        peer_id: offer.peer_id.clone(),
        answer_sdp: valid_sdp(),
        ..Default::default()
    }
}

/// A synthetic 64-hex fingerprint distinct per index.
pub fn synthetic_fingerprint(index: usize, fill: char) -> String {
    format!("{index:02x}{}", fill.to_string().repeat(62))
}

/// A synthetic valid peer id distinct per index.
pub fn synthetic_peer_id(index: usize) -> String {
    format!("00000000-0000-4000-8000-{index:012}")
}
