//! A real in-process str0m pair: a browser-side offerer on loopback and this
//! worker's terminal peer owner with the production str0m transport. The
//! owner answers over its own UDP socket, DTLS and SCTP come up, the worker's
//! terminal frame arrives framed on the terminal lane, and the browser's
//! control frame reaches the socket owner's ingress. v2 proved this path only
//! in a real browser (`smoke/terminal/terminal-peer.spec.ts`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "peer_support/offerer.rs"]
mod offerer;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use offerer::BrowserOfferer;
use roost_proto::DLocalTerminalPeerOffer;
use roost_protocol::terminal_peer::packets::{
    TerminalPeerPacketHeader, TerminalPeerPacketLane as Lane, encode_terminal_peer_packet,
    parse_terminal_peer_packet,
};
use roost_protocol::terminal_peer::peer::TERMINAL_PEER_DATA_CHANNELS;
use roost_worker::local_terminal::PeerGrantAuthorization;
use roost_worker::local_terminal::{ExpectedPeer, PacketSendResult, TerminalPacketPort};
use roost_worker::peer::native::{NativeChannelSpec, NativePeerEvent, str0m_loader};
use roost_worker::peer::{
    PeerBootstrapState, PeerTransportConfig, TerminalPeerOwner, TerminalPeerOwnerDeps,
    TerminalPeerPacketBudget, TerminalPeerPacketIngress, TerminalPeerPacketPort,
};
use roost_worker::uplink::{RequestBudget, Uplink};
use tokio::sync::mpsc;

const WORKER_EPOCH: &str = "11111111-1111-4111-8111-111111111111";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct ForwardingIngress(mpsc::UnboundedSender<Vec<u8>>);

impl TerminalPeerPacketIngress for ForwardingIngress {
    fn on_message(&self, bytes: &[u8]) {
        let _ = self.0.send(bytes.to_vec());
    }
    fn on_close(&self) {}
}

fn terminal_channels() -> Vec<NativeChannelSpec> {
    TERMINAL_PEER_DATA_CHANNELS
        .iter()
        .map(|definition| NativeChannelSpec {
            id: u16::from(definition.id),
            label: definition.label.to_owned(),
            ordered: true,
            protocol: definition.protocol.to_owned(),
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_in_process_str0m_pair_carries_a_terminal_frame_both_ways() {
    let ports: Arc<Mutex<Vec<Arc<TerminalPeerPacketPort>>>> = Arc::default();
    let (ingress_in, mut ingress) = mpsc::unbounded_channel();
    let opened = Arc::clone(&ports);
    let owner = TerminalPeerOwner::new(TerminalPeerOwnerDeps {
        process_epoch: WORKER_EPOCH.into(),
        transport: PeerTransportConfig {
            enabled: true,
            bind_address: Some("127.0.0.1".parse().unwrap()),
            port_range: None,
        },
        is_current_coordinator: Arc::new(|_: &str| true),
        authorize_grant: Arc::new(|_: &DLocalTerminalPeerOffer| PeerGrantAuthorization::Authorized),
        open_peer_port: Arc::new(
            move |port: Arc<TerminalPeerPacketPort>, _expected: ExpectedPeer| {
                lock(&opened).push(port);
                Some(Arc::new(ForwardingIngress(ingress_in.clone()))
                    as Arc<dyn TerminalPeerPacketIngress>)
            },
        ),
        native_loader: str0m_loader(),
        packet_budget: TerminalPeerPacketBudget::new(),
        test_faults: None,
        expire_grant: Arc::new(|_: &str| {}),
        runtime: tokio::runtime::Handle::current(),
    });
    assert_eq!(owner.bootstrap().await, PeerBootstrapState::Ready);

    let (mut browser, offer_sdp) = BrowserOfferer::start(&terminal_channels()).await;
    let request = DLocalTerminalPeerOffer {
        request_id: "request-1".into(),
        connection_generation: "generation-1".into(),
        worker_epoch: WORKER_EPOCH.into(),
        grant_id: "grant-1".into(),
        peer_id: "00000000-0000-4000-8000-000000000001".into(),
        device_fingerprint: "device".into(),
        tab_id: "tab".into(),
        offer_sdp,
        budget_ms: 8_000,
        ..Default::default()
    };
    let answer = owner
        .offer(
            request,
            RequestBudget::from_budget_ms(8_000, Instant::now()),
            Uplink::detached().fence(),
        )
        .await
        .expect("the worker answers the browser's offer");
    browser.accept_answer(&answer.answer_sdp);
    for _ in 0..3 {
        browser
            .next_matching(|event| matches!(event, NativePeerEvent::ChannelOpen(_)))
            .await;
    }
    let port = Arc::clone(&lock(&ports)[0]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !port.is_open() {
        assert!(
            Instant::now() < deadline,
            "the worker's control channel opened"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let frame = b"\x1b[1mterminal frame\x1b[0m".to_vec();
    assert_ne!(
        port.send(frame.clone(), Lane::Terminal),
        PacketSendResult::Refused
    );
    let NativePeerEvent::ChannelMessage { channel, data, .. } = browser
        .next_matching(|event| matches!(event, NativePeerEvent::ChannelMessage { .. }))
        .await
    else {
        unreachable!("next_matching returns a message");
    };
    assert_eq!(channel, Lane::Terminal as usize);
    let packet = parse_terminal_peer_packet(Lane::Terminal, &data).unwrap();
    assert_eq!(
        (packet.header.message_id, packet.payload),
        (1, frame.as_slice())
    );

    let hello = b"hello from the browser".to_vec();
    let header = TerminalPeerPacketHeader {
        message_id: 1,
        total_bytes: hello.len() as u32,
        offset_bytes: 0,
    };
    browser.send(
        Lane::Control as usize,
        &encode_terminal_peer_packet(Lane::Control, header, &hello).unwrap(),
    );
    let received = tokio::time::timeout(Duration::from_secs(10), ingress.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received, hello);
    assert_eq!(owner.established_count(), 1);
    owner.dispose();
    assert!(!port.is_open());
}
