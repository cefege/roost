// A deterministic native-transport fake for the peer owner, connection and
// packet-port suites: native callback ordering and peer state without a UDP
// socket or a DTLS handshake. Ports v2
// `apps/worker/tests/terminal/peer/terminal-peer-owner-fixture.ts` and the
// fake channels of `terminal-peer-packet-port.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use roost_worker::peer::native::{
    NativeFingerprint, NativeLoader, NativePeer, NativePeerConfig, NativePeerError,
    NativePeerEvent, NativePeerEvents, NativePeerFactory,
};
use roost_worker::uplink::OwnerFuture;
use tokio::sync::{Semaphore, mpsc};

pub const OFFER_FINGERPRINT: &str = "00:01:02:03:04:05:06:07:08:09:0a:0b:0c:0d:0e:0f:\
10:11:12:13:14:15:16:17:18:19:1a:1b:1c:1d:1e:1f";
pub const ICE_PASSWORD: &str = "pppppppppppppppppppppp";

pub fn offer_sdp() -> String {
    [
        "v=0",
        "o=- 1 2 IN IP4 127.0.0.1",
        "s=-",
        "t=0 0",
        "m=application 9 UDP/DTLS/SCTP webrtc-datachannel",
        "a=setup:actpass",
        &format!("a=fingerprint:sha-256 {OFFER_FINGERPRINT}"),
        "a=ice-ufrag:offer-ufrag",
        &format!("a=ice-pwd:{ICE_PASSWORD}"),
        "a=max-message-size:16384",
        "a=candidate:host 1 udp 2122260223 192.0.2.8 5000 typ host",
        "",
    ]
    .join("\r\n")
}

pub fn answer_sdp() -> String {
    offer_sdp().replace("a=setup:actpass", "a=setup:active")
}

/// v2 `createFakeNativeFixture()`.
#[derive(Debug, Default)]
pub struct FakeNative {
    peers: Mutex<Vec<Arc<FakePeer>>>,
    events: Arc<Mutex<Vec<String>>>,
    cleanup_calls: AtomicUsize,
    defer_new_peers: AtomicBool,
    fail_after_answer: AtomicBool,
}

impl FakeNative {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn loader(self: &Arc<Self>) -> NativeLoader {
        let fake = Arc::clone(self);
        Arc::new(move || {
            let fake = Arc::clone(&fake);
            Box::pin(async move { Ok(fake as Arc<dyn NativePeerFactory>) })
        })
    }

    /// A loader that resolves only once `gate` gets a permit (v2 `nativeGate`).
    pub fn gated_loader(self: &Arc<Self>, gate: Arc<Semaphore>) -> NativeLoader {
        let fake = Arc::clone(self);
        Arc::new(move || {
            let (fake, gate) = (Arc::clone(&fake), Arc::clone(&gate));
            Box::pin(async move {
                gate.acquire().await.unwrap().forget();
                Ok(fake as Arc<dyn NativePeerFactory>)
            })
        })
    }

    pub fn failing_loader() -> NativeLoader {
        Arc::new(|| Box::pin(async { Err(NativePeerError::Unavailable) }))
    }

    pub fn peers(&self) -> Vec<Arc<FakePeer>> {
        self.peers.lock_or_recover().clone()
    }

    pub fn events(&self) -> Vec<String> {
        self.events.lock_or_recover().clone()
    }

    pub fn push_event(&self, event: impl Into<String>) {
        self.events.lock_or_recover().push(event.into());
    }

    pub fn cleanup_calls(&self) -> usize {
        self.cleanup_calls.load(Ordering::SeqCst)
    }

    pub fn set_defer_new_peers(&self, defer: bool) {
        self.defer_new_peers.store(defer, Ordering::SeqCst);
    }

    /// v2 `afterAnswerSettled = () => peer.emitIceFailure()` for every new peer.
    pub fn set_fail_after_answer(&self, fail: bool) {
        self.fail_after_answer.store(fail, Ordering::SeqCst);
    }
}

impl NativePeerFactory for FakeNative {
    fn create(
        &self,
        config: NativePeerConfig,
    ) -> Result<(Arc<dyn NativePeer>, NativePeerEvents), NativePeerError> {
        let (peer, events) = FakePeer::with_events(config, Arc::clone(&self.events));
        peer.defer_gathering.store(
            self.defer_new_peers.load(Ordering::SeqCst),
            Ordering::SeqCst,
        );
        peer.fail_after_answer.store(
            self.fail_after_answer.load(Ordering::SeqCst),
            Ordering::SeqCst,
        );
        self.peers.lock_or_recover().push(Arc::clone(&peer));
        let peer: Arc<dyn NativePeer> = peer;
        Ok((peer, events))
    }

    fn cleanup(&self) {
        self.cleanup_calls.fetch_add(1, Ordering::SeqCst);
    }
}

/// One fake native peer and its channels (v2 `FakePeerConnection` +
/// `FakeDataChannel`/`FakeNativeChannel`).
#[derive(Debug)]
pub struct FakePeer {
    config: NativePeerConfig,
    events_log: Arc<Mutex<Vec<String>>>,
    sender: mpsc::UnboundedSender<NativePeerEvent>,
    open: Mutex<Vec<bool>>,
    buffered: Mutex<Vec<usize>>,
    thresholds: Mutex<Vec<usize>>,
    send_results: Mutex<Vec<VecDeque<bool>>>,
    sent: Mutex<Vec<(usize, Vec<u8>)>>,
    closed: AtomicBool,
    defer_gathering: AtomicBool,
    fail_after_answer: AtomicBool,
    gathering: Arc<Semaphore>,
    remote_fingerprint_calls: AtomicUsize,
}

impl FakePeer {
    /// A peer with `channels` channels and no factory, for packet-port suites.
    pub fn standalone(channels: usize) -> Arc<Self> {
        let config = NativePeerConfig {
            name: "fake".into(),
            stun_urls: Vec::new(),
            bind_address: None,
            port_range: None,
            max_message_size: 16 * 1024,
            channels: Vec::new(),
        };
        Self::build(config, Arc::default(), channels).0
    }

    fn with_events(
        config: NativePeerConfig,
        events_log: Arc<Mutex<Vec<String>>>,
    ) -> (Arc<Self>, NativePeerEvents) {
        let channels = config.channels.len();
        Self::build(config, events_log, channels)
    }

    fn build(
        config: NativePeerConfig,
        events_log: Arc<Mutex<Vec<String>>>,
        channels: usize,
    ) -> (Arc<Self>, NativePeerEvents) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let peer = Arc::new(Self {
            config,
            events_log,
            sender,
            open: Mutex::new(vec![false; channels]),
            buffered: Mutex::new(vec![0; channels]),
            thresholds: Mutex::new(vec![0; channels]),
            send_results: Mutex::new(vec![VecDeque::new(); channels]),
            sent: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
            defer_gathering: AtomicBool::new(false),
            fail_after_answer: AtomicBool::new(false),
            gathering: Arc::new(Semaphore::new(0)),
            remote_fingerprint_calls: AtomicUsize::new(0),
        });
        (peer, receiver)
    }

    pub fn config(&self) -> NativePeerConfig {
        self.config.clone()
    }

    /// Delivers a native callback to the connection's event pump.
    pub fn emit(&self, event: NativePeerEvent) {
        let _ = self.sender.send(event);
    }

    /// v2 `channel.emitOpen()` through the event pump.
    pub fn open_channel(&self, channel: usize) {
        self.set_open(channel, true);
        self.emit(NativePeerEvent::ChannelOpen(channel));
    }

    pub fn set_open(&self, channel: usize, open: bool) {
        self.open.lock_or_recover()[channel] = open;
    }

    pub fn set_buffered(&self, channel: usize, bytes: usize) {
        self.buffered.lock_or_recover()[channel] = bytes;
    }

    /// v2 `returnValues.push(value)`.
    pub fn push_send_result(&self, channel: usize, sent_now: bool) {
        self.send_results.lock_or_recover()[channel].push_back(sent_now);
    }

    pub fn sent(&self, channel: usize) -> Vec<Vec<u8>> {
        self.sent
            .lock_or_recover()
            .iter()
            .filter(|(lane, _)| *lane == channel)
            .map(|(_, bytes)| bytes.clone())
            .collect()
    }

    pub fn sent_order(&self) -> Vec<usize> {
        self.sent
            .lock_or_recover()
            .iter()
            .map(|(lane, _)| *lane)
            .collect()
    }

    pub fn threshold(&self, channel: usize) -> usize {
        self.thresholds.lock_or_recover()[channel]
    }

    pub fn complete_gathering(&self) {
        self.gathering.add_permits(1);
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn remote_fingerprint_calls(&self) -> usize {
        self.remote_fingerprint_calls.load(Ordering::SeqCst)
    }
}

impl NativePeer for FakePeer {
    fn answer(
        &self,
        _offer_sdp: String,
        _deadline: Duration,
    ) -> OwnerFuture<Result<String, NativePeerError>> {
        self.events_log
            .lock_or_recover()
            .push("remote-description".into());
        let defer = self.defer_gathering.load(Ordering::SeqCst);
        let fail_after = self.fail_after_answer.load(Ordering::SeqCst);
        let gathering = Arc::clone(&self.gathering);
        let sender = self.sender.clone();
        Box::pin(async move {
            if defer {
                gathering.acquire().await.unwrap().forget();
            }
            if fail_after {
                let _ = sender.send(NativePeerEvent::Failed);
                for _ in 0..8 {
                    tokio::task::yield_now().await;
                }
            }
            Ok(answer_sdp())
        })
    }

    fn remote_fingerprint(&self) -> Option<NativeFingerprint> {
        self.remote_fingerprint_calls.fetch_add(1, Ordering::SeqCst);
        Some(NativeFingerprint {
            algorithm: "sha-256".into(),
            value: OFFER_FINGERPRINT.into(),
        })
    }

    fn send(&self, channel: usize, bytes: &[u8]) -> Result<bool, NativePeerError> {
        self.sent.lock_or_recover().push((channel, bytes.to_vec()));
        Ok(self.send_results.lock_or_recover()[channel]
            .pop_front()
            .unwrap_or(true))
    }

    fn buffered_amount(&self, channel: usize) -> usize {
        self.buffered.lock_or_recover()[channel]
    }

    fn set_buffered_amount_low_threshold(&self, channel: usize, bytes: usize) {
        self.thresholds.lock_or_recover()[channel] = bytes;
    }

    fn is_open(&self, channel: usize) -> bool {
        !self.is_closed() && self.open.lock_or_recover()[channel]
    }

    fn close_channel(&self, channel: usize) {
        self.set_open(channel, false);
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

/// Locks a fixture mutex; a panicking test poisons nothing worth refusing.
trait LockOrRecover<T> {
    fn lock_or_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> LockOrRecover<T> for Mutex<T> {
    fn lock_or_recover(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
