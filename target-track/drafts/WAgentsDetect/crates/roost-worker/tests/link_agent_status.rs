//! The volatile agent-status lane of the coordinator link: full identity
//! encoding, replacement repair while frames wait, and replay of possibly-lost
//! retirements across the snapshot/reconnect barrier. Ports v2
//! `apps/worker/tests/transport/coord-link-agent-status.test.ts`; the last two
//! cases run a real `LinkLoop` against a loopback coordinator.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod link_downstream_support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use link_downstream_support::live::{LiveLink, Socket, eventually, next_bytes, next_frame, send, SNAPSHOT};
use link_downstream_support::{Fakes, OwnerMode};
use roost_protocol::proto_adapters::coord_worker_proto::decode_upstream;
use roost_protocol::wire::agent_status::{
    AgentId, AgentOccupantId, AgentRuntimeState as State, AgentStatusFields, AgentStatusSource as Source,
    AgentStatusUpdate, StatusEpoch,
};
use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream as Down, CoordWorkerUpstream as Up, EventAck};
use roost_worker::agents::registry::AgentStatusPublisher;
use roost_worker::agents::status_stack::UplinkAgentStatusPublisher;
use roost_worker::link_ports::{DownstreamOwners, LinkLifecyclePort, LinkLifecycles};
use roost_worker::runtime::link_loop::AdmitRefusal;
use roost_worker::runtime::link_loop::agent_status::{AgentStatusOutbox, EncodedAgentStatus};
use roost_worker::runtime::link_wire::ProtoLinkWire;
use roost_worker::uplink::Uplink;

const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";
const STATUS_EPOCH: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OCCUPANT_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa1";
const OCCUPANT_B: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbb2";
const OCCUPANT_C: &str = "cccccccc-cccc-4ccc-8ccc-ccccccccccc3";

fn status(occupant: &str, revision: i64, active: bool, state: State, source: Source) -> AgentStatusUpdate {
    AgentStatusUpdate {
        common: AgentStatusFields {
            session_id: SessionId::try_from(SESSION_ID).unwrap(),
            agent_id: AgentId::try_from("omp").unwrap(),
            state,
            message: None,
            revision,
            completed_revision: 0,
            updated_at: 1_000 + revision,
            status_epoch: Some(StatusEpoch::try_from(STATUS_EPOCH).unwrap()),
            occupant_id: Some(AgentOccupantId::try_from(occupant).unwrap()),
            source: Some(source),
            occupant_exited: false,
        },
        active,
    }
}

fn working(occupant: &str, revision: i64, active: bool) -> AgentStatusUpdate {
    status(occupant, revision, active, State::Working, Source::Integration)
}

fn encoded(status: &AgentStatusUpdate) -> EncodedAgentStatus {
    EncodedAgentStatus::encode(status, &ProtoLinkWire).unwrap()
}

/// Everything the outbox would write now, in order.
fn drain(outbox: &mut AgentStatusOutbox) -> Vec<AgentStatusUpdate> {
    let mut written = Vec::new();
    while let Some(bytes) = outbox.next_bytes() {
        written.push(decoded(bytes));
        outbox.commit_written();
    }
    written
}

fn decoded(bytes: &[u8]) -> AgentStatusUpdate {
    match decode_upstream(bytes).unwrap() {
        Up::AgentStatus(frame) => AgentStatusUpdate {
            common: frame.status.common,
            active: frame.status.active,
        },
        other => panic!("unexpected frame {}", other.kind()),
    }
}

fn identities(statuses: &[AgentStatusUpdate]) -> Vec<(String, bool)> {
    statuses
        .iter()
        .map(|status| (status.common.occupant_id.clone().unwrap().as_str().to_owned(), status.active))
        .collect()
}

fn expect(pairs: &[(&str, bool)]) -> Vec<(String, bool)> {
    pairs.iter().map(|(occupant, active)| ((*occupant).to_owned(), *active)).collect()
}

#[test]
fn backpressure_preserves_sent_retirement_and_elides_unseen_replacement_occupants() {
    let mut outbox = AgentStatusOutbox::default();
    outbox.queue(encoded(&working(OCCUPANT_A, 1, true)));
    let mut sent = drain(&mut outbox);
    for update in [
        status(OCCUPANT_A, 2, true, State::Blocked, Source::Integration),
        status(OCCUPANT_A, 3, false, State::Blocked, Source::Integration),
        working(OCCUPANT_B, 4, true),
        working(OCCUPANT_B, 5, false),
        status(OCCUPANT_C, 6, true, State::Idle, Source::Screen),
    ] {
        outbox.queue(encoded(&update));
    }
    assert!(outbox.has_pending());
    sent.extend(drain(&mut outbox));
    assert!(!outbox.has_pending());
    assert_eq!(identities(&sent), expect(&[(OCCUPANT_A, true), (OCCUPANT_A, false), (OCCUPANT_C, true)]));
    assert_eq!(sent[1].common.status_epoch.as_ref().unwrap().as_str(), STATUS_EPOCH);
    assert_eq!(sent[1].common.source, Some(Source::Integration));
    assert_eq!(sent[1].common.revision, 3);
    assert_eq!(sent[2].common.source, Some(Source::Screen));
    assert_eq!(sent[2].common.revision, 6);
}

#[test]
fn an_occupant_that_never_reached_the_socket_needs_no_retirement_frame() {
    let mut outbox = AgentStatusOutbox::default();
    outbox.queue(encoded(&working(OCCUPANT_A, 1, true)));
    outbox.queue(encoded(&working(OCCUPANT_A, 2, false)));
    outbox.queue(encoded(&status(OCCUPANT_B, 3, true, State::Blocked, Source::Integration)));
    assert_eq!(identities(&drain(&mut outbox)), expect(&[(OCCUPANT_B, true)]));
}

#[test]
fn new_worker_transport_rejects_identityless_status_instead_of_emitting_legacy_frames() {
    let mut legacy = working(OCCUPANT_A, 1, true);
    legacy.common.status_epoch = None;
    legacy.common.occupant_id = None;
    legacy.common.source = None;
    let refused = EncodedAgentStatus::encode(&legacy, &ProtoLinkWire);
    assert!(matches!(refused, Err(AdmitRefusal::UnidentifiedAgentStatus)));
}

/// v2's `onSnapshotReady` in these tests: republish whatever is current.
#[derive(Debug, Default)]
struct ResendOnSnapshot {
    uplink: Mutex<Option<Uplink>>,
    current: Mutex<Option<AgentStatusUpdate>>,
}

impl LinkLifecyclePort for ResendOnSnapshot {
    fn on_open(&self) {}
    fn on_hello_ack(&self, _: bool) {}
    fn on_detach(&self) {}
    fn on_writable(&self) {}
    fn on_snapshot_ready(&self) {
        let current = self.current.lock().unwrap().clone();
        if let (Some(status), Some(uplink)) = (current, self.uplink.lock().unwrap().clone()) {
            UplinkAgentStatusPublisher::new(uplink).publish(status);
        }
    }
}

async fn start_link() -> (LiveLink, Fakes, Arc<ResendOnSnapshot>) {
    let fakes = Fakes::new(OwnerMode::Answer);
    let resend = Arc::new(ResendOnSnapshot::default());
    let owners = fakes.owners();
    let lifecycles: Vec<Arc<dyn LinkLifecyclePort>> = vec![Arc::clone(&owners.lifecycle), Arc::clone(&resend) as _];
    let live = LiveLink::start_with(
        DownstreamOwners {
            lifecycle: Arc::new(LinkLifecycles::new(lifecycles)),
            ..owners
        },
        None,
    )
    .await;
    *resend.uplink.lock().unwrap() = Some(live.uplink.clone());
    (live, fakes, resend)
}

/// `go_live` for a connection whose snapshot takes sequence `snapshot_seq`.
async fn go_live_at(socket: &mut Socket, snapshot_seq: u64) {
    assert!(matches!(next_frame(socket).await, Up::Hello { .. }));
    send(socket, &Down::HelloAck { capabilities: Vec::new(), trace_id: None }).await;
    assert_eq!(next_bytes(socket).await, SNAPSHOT);
    send(socket, &Down::EventAck(EventAck { client_seq: snapshot_seq })).await;
}

async fn statuses(socket: &mut Socket, count: usize) -> Vec<AgentStatusUpdate> {
    let mut read = Vec::with_capacity(count);
    for _ in 0..count {
        read.push(decoded(&next_bytes(socket).await));
    }
    read
}

async fn assert_quiet(socket: &mut Socket) {
    let extra = tokio::time::timeout(Duration::from_millis(300), next_bytes(socket)).await;
    assert!(extra.is_err(), "nothing else was written");
}

async fn reconnect(live: &LiveLink, fakes: &Fakes, socket: Socket, detaches: usize) -> Socket {
    let mut socket = socket;
    socket.close(None).await.unwrap();
    drop(socket);
    eventually(|| fakes.log.calls().iter().filter(|call| *call == "lifecycle.on_detach").count() >= detaches).await;
    live.accept().await
}

#[tokio::test(flavor = "multi_thread")]
async fn successful_retirements_replay_in_order_without_an_active_snapshot() {
    let (live, fakes, resend) = start_link().await;
    let publisher = UplinkAgentStatusPublisher::new(live.uplink.clone());
    let mut first = live.accept().await;
    go_live_at(&mut first, 1).await;
    publisher.publish(working(OCCUPANT_A, 1, true));
    publisher.publish(working(OCCUPANT_A, 2, false));
    let sent = statuses(&mut first, 2).await;
    assert_eq!(identities(&sent), expect(&[(OCCUPANT_A, true), (OCCUPANT_A, false)]));
    publisher.publish(working(OCCUPANT_B, 3, true));
    publisher.publish(working(OCCUPANT_B, 4, false));
    statuses(&mut first, 2).await;

    let mut second = reconnect(&live, &fakes, first, 1).await;
    go_live_at(&mut second, 2).await;
    let replayed = statuses(&mut second, 2).await;
    assert_eq!(identities(&replayed), expect(&[(OCCUPANT_A, false), (OCCUPANT_B, false)]));
    assert_quiet(&mut second).await;

    let current = working(OCCUPANT_C, 5, true);
    *resend.current.lock().unwrap() = Some(current.clone());
    publisher.publish(current);
    statuses(&mut second, 1).await;
    let mut third = reconnect(&live, &fakes, second, 2).await;
    go_live_at(&mut third, 3).await;
    let replayed = statuses(&mut third, 3).await;
    assert_eq!(
        identities(&replayed),
        expect(&[(OCCUPANT_A, false), (OCCUPANT_B, false), (OCCUPANT_C, true)])
    );
    live.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pending_replacement_survives_socket_detach_and_reconnect_resend() {
    let (live, fakes, resend) = start_link().await;
    let publisher = UplinkAgentStatusPublisher::new(live.uplink.clone());
    let mut first = live.accept().await;
    go_live_at(&mut first, 1).await;
    publisher.publish(working(OCCUPANT_A, 1, true));
    statuses(&mut first, 1).await;

    first.close(None).await.unwrap();
    drop(first);
    eventually(|| fakes.log.calls().iter().any(|call| call == "lifecycle.on_detach")).await;
    // Published while no socket can take them: they wait, compacted.
    publisher.publish(working(OCCUPANT_A, 2, false));
    publisher.publish(working(OCCUPANT_B, 3, true));
    *resend.current.lock().unwrap() = Some(working(OCCUPANT_B, 3, true));

    let mut second = live.accept().await;
    go_live_at(&mut second, 2).await;
    let reconnected = statuses(&mut second, 2).await;
    assert_eq!(identities(&reconnected), expect(&[(OCCUPANT_A, false), (OCCUPANT_B, true)]));
    assert_eq!(reconnected[1].common.status_epoch.as_ref().unwrap().as_str(), STATUS_EPOCH);
    assert_eq!(reconnected[1].common.revision, 3);
    assert_quiet(&mut second).await;
    live.stop().await;
}
