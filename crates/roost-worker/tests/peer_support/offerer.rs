// The browser side of an in-process peer pair: a str0m offerer on loopback
// that creates the same negotiated channels a browser document does, applies
// the worker's answer, and reports channel traffic as `NativePeerEvent`s.
// Used by the terminal and attachment peer suites to prove a real DTLS/SCTP
// path end to end (v2 proved it in a real browser only).
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use roost_worker::peer::native::{NativeChannelSpec, NativePeerEvent};
use str0m::change::{SdpAnswer, SdpPendingOffer};
use str0m::channel::{ChannelConfig, ChannelId};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use tokio::net::UdpSocket;
use tokio::sync::{Notify, mpsc};

/// How long a suite waits for one offerer event before it fails.
const EVENT_DEADLINE: Duration = Duration::from_secs(10);

struct Shared {
    rtc: Mutex<Rtc>,
    channels: Vec<ChannelId>,
    wake: Notify,
}

impl Shared {
    fn rtc(&self) -> MutexGuard<'_, Rtc> {
        self.rtc.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub struct BrowserOfferer {
    shared: Arc<Shared>,
    pending: Option<SdpPendingOffer>,
    events: mpsc::UnboundedReceiver<NativePeerEvent>,
    driver: tokio::task::JoinHandle<()>,
}

impl BrowserOfferer {
    /// Binds loopback, creates `channels` as negotiated channels, and returns
    /// the offer a browser would send.
    pub async fn start(channels: &[NativeChannelSpec]) -> (Self, String) {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let local = socket.local_addr().unwrap();
        let mut rtc = RtcConfig::new().build(Instant::now());
        rtc.add_local_candidate(Candidate::host(local, "udp").unwrap())
            .unwrap();
        let mut api = rtc.sdp_api();
        let ids: Vec<ChannelId> = channels
            .iter()
            .map(|spec| {
                api.add_channel_with_config(ChannelConfig {
                    label: spec.label.clone(),
                    ordered: spec.ordered,
                    negotiated: Some(spec.id),
                    protocol: spec.protocol.clone(),
                    ..ChannelConfig::default()
                })
            })
            .collect();
        let (offer, pending) = api.apply().unwrap();
        let shared = Arc::new(Shared {
            rtc: Mutex::new(rtc),
            channels: ids,
            wake: Notify::new(),
        });
        let (events_in, events) = mpsc::unbounded_channel();
        let driver = tokio::spawn(drive(Arc::clone(&shared), socket, local, events_in));
        (
            Self {
                shared,
                pending: Some(pending),
                events,
                driver,
            },
            offer.to_sdp_string(),
        )
    }

    pub fn accept_answer(&mut self, answer_sdp: &str) {
        let answer = SdpAnswer::from_sdp_string(answer_sdp).unwrap();
        let pending = self.pending.take().unwrap();
        self.shared
            .rtc()
            .sdp_api()
            .accept_answer(pending, answer)
            .unwrap();
        self.shared.wake.notify_one();
    }

    pub fn send(&self, channel: usize, bytes: &[u8]) {
        let id = self.shared.channels[channel];
        let mut rtc = self.shared.rtc();
        let accepted = rtc
            .channel(id)
            .expect("the offerer channel is open")
            .write(true, bytes)
            .unwrap();
        assert!(accepted, "the offerer's SCTP buffer took the message");
        drop(rtc);
        self.shared.wake.notify_one();
    }

    /// Close one channel as a browser's `RTCDataChannel.close()` does: an SCTP
    /// stream reset, the transport left up.
    pub fn close_channel(&self, channel: usize) {
        let id = self.shared.channels[channel];
        self.shared.rtc().direct_api().close_data_channel(id);
        self.shared.wake.notify_one();
    }

    /// Close the connection as a browser's `RTCPeerConnection.close()` does:
    /// SCTP shutdown and DTLS `close_notify`, sent without waiting for replies.
    pub fn close(&self) {
        self.shared.rtc().close().unwrap();
        self.shared.wake.notify_one();
    }

    pub async fn next_event(&mut self) -> NativePeerEvent {
        tokio::time::timeout(EVENT_DEADLINE, self.events.recv())
            .await
            .expect("an offerer event in time")
            .expect("the offerer is running")
    }

    /// The next event `accept` keeps, skipping the rest.
    pub async fn next_matching(
        &mut self,
        accept: impl Fn(&NativePeerEvent) -> bool,
    ) -> NativePeerEvent {
        loop {
            let event = self.next_event().await;
            if accept(&event) {
                return event;
            }
        }
    }
}

impl Drop for BrowserOfferer {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

async fn drive(
    shared: Arc<Shared>,
    socket: UdpSocket,
    local: SocketAddr,
    events: mpsc::UnboundedSender<NativePeerEvent>,
) {
    let mut buffer = vec![0u8; 2048];
    let mut received: Option<(SocketAddr, Vec<u8>)> = None;
    loop {
        let (transmits, deadline) = {
            let mut rtc = shared.rtc();
            let now = Instant::now();
            let fed = match received.take() {
                Some((source, bytes)) => match bytes.as_slice().try_into() {
                    Ok(contents) => rtc.handle_input(Input::Receive(
                        now,
                        Receive {
                            proto: Protocol::Udp,
                            source,
                            destination: local,
                            contents,
                        },
                    )),
                    Err(_) => Ok(()),
                },
                None => rtc.handle_input(Input::Timeout(now)),
            };
            fed.unwrap();
            let mut transmits = Vec::new();
            let deadline = loop {
                match rtc.poll_output().unwrap() {
                    Output::Transmit(transmit) => transmits.push(transmit),
                    Output::Timeout(deadline) => break deadline,
                    Output::Event(event) => {
                        if let Some(event) = translate(&shared.channels, event) {
                            let _ = events.send(event);
                        }
                    }
                }
            };
            (transmits, deadline)
        };
        for transmit in transmits {
            socket
                .send_to(&transmit.contents, transmit.destination)
                .await
                .unwrap();
        }
        tokio::select! {
            read = socket.recv_from(&mut buffer) => {
                let (length, source) = read.unwrap();
                received = Some((source, buffer[..length].to_vec()));
            }
            () = tokio::time::sleep_until(deadline.into()) => {}
            () = shared.wake.notified() => {}
        }
    }
}

fn translate(channels: &[ChannelId], event: Event) -> Option<NativePeerEvent> {
    let index = |id: ChannelId| channels.iter().position(|known| *known == id);
    match event {
        Event::Connected => Some(NativePeerEvent::Connected),
        Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
            Some(NativePeerEvent::Failed)
        }
        Event::ChannelOpen(id, _) => index(id).map(NativePeerEvent::ChannelOpen),
        Event::ChannelData(data) => index(data.id).map(|channel| NativePeerEvent::ChannelMessage {
            channel,
            binary: data.binary,
            data: data.data,
        }),
        Event::ChannelClose(id) => index(id).map(NativePeerEvent::ChannelClosed),
        _ => None,
    }
}
