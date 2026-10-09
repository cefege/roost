//! The real grant store, socket owner, view owner, cell cadence and session
//! manager over an auto-answering keeper, with the carrier stubbed: what the
//! `local_terminal_*` suites drive. Mirrors the fixture of v2
//! `apps/worker/tests/local-door/local-terminal-socket.test.ts` (its
//! `installAutoKeeper` + stream harness + stub WebSocket).
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

mod stub_port;

pub use stub_port::StubPort;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use roost_keeper::client_resize::ResizeOutcome;
use roost_keeper::frames::ChannelBinding as KeeperChannel;
use roost_keeper::payloads::TerminalState;
use roost_observability::clock::{EventClock, SystemClock};
use roost_proto::__buffa::oneof::local_terminal_client_frame::Frame as ClientFrame;
use roost_proto::__buffa::oneof::local_terminal_server_frame::Frame as ServerFrame;
use roost_proto::buffa::Message;
use roost_proto::{
    DLocalTerminalGrant, InputCommand, LocalTerminalClientFrame, LocalTerminalHello,
    TerminalViewCommand,
};
use roost_term::RioCore;
use roost_worker::local_terminal::{
    LocalTerminalDoor, LocalTerminalDoorDeps, LocalTerminalGrantStore, LocalTerminalSockets,
    TerminalPacketPort,
};
use roost_worker::runtime::cell_cadence::CellCadence;
use roost_worker::runtime::link_loop::CoordinatorCellSink;
use roost_worker::runtime::link_wire::ProtoLinkWire;
use roost_worker::session::cell_sink::{CellSink, local_cell_sink_id};
use roost_worker::session::keeper_channels::{
    KeeperChannels, KeeperFault, KeeperInputCommand, KeeperInputResult, SurvivorHistory,
};
use roost_worker::session::sinks::ChannelBinding;
use roost_worker::terminal_input::{TerminalInputRouteOwner, TerminalInputWorkBudget};
use roost_worker::terminal_view::{SessionViewPort, TerminalViewOwner, TerminalViewOwnerDeps};
use roost_worker::uplink::{UplinkReceiver, channel};
use sha2::{Digest, Sha256};

use super::terminal_stream_support::{COLS, Harness, ROWS, SESSION, held};

pub const DEVICE: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
pub const TAB: &str = "tab-local";
pub const WORKER_FP: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
pub const WORKER_EPOCH: &str = "11111111-1111-4111-8111-111111111111";
/// In the grant, but no live session: input must consult the real write path
/// instead of answering from the grant alone.
pub const GRANTED_DEAD_SESSION: &str = "22222222-3333-4333-8444-555555555555";
pub const UNGRANTED_SESSION: &str = "33333333-4444-4333-8444-555555555555";
pub const SECRET: &str = "5ec2e75ec2e75ec2e75ec2e75ec2e75ec2e75ec2e75ec2e75ec2e75ec2e75ec2";
pub const GRANT_ID: &str = "0f0f0f0f-0f0f-4f0f-8f0f-0f0f0f0f0f0f";

/// v2 `installAutoKeeper`: every resize applies, every input is acknowledged
/// in full unless the test scripted another answer; the bytes are kept.
#[derive(Default)]
pub struct AutoKeeper {
    pub written: Mutex<Vec<u8>>,
    pub answers: Mutex<VecDeque<KeeperInputResult>>,
    state: Mutex<Option<TerminalState>>,
}

impl KeeperChannels for AutoKeeper {
    fn live_channels(&self) -> Result<Vec<KeeperChannel>, KeeperFault> {
        Ok(Vec::new())
    }
    fn channel_history(&self, _channel_id: u16) -> Result<SurvivorHistory, KeeperFault> {
        Err(KeeperFault {
            operation: "channel_history",
            reason: "ordered history is unavailable".to_owned(),
        })
    }
    fn terminal_state(&self, _channel_id: u16) -> Result<TerminalState, KeeperFault> {
        Ok(held(&self.state).unwrap_or(TerminalState {
            applied_seq: 0,
            cols: COLS,
            rows: ROWS,
        }))
    }
    fn reattach_with_history(
        &self,
        channel_id: u16,
        _pid: u32,
        _binding: Arc<dyn ChannelBinding>,
    ) -> Result<SurvivorHistory, KeeperFault> {
        self.channel_history(channel_id)
    }
    fn kill_channel(&self, _channel_id: u16) -> Result<(), KeeperFault> {
        Ok(())
    }
    fn resize_channel(
        &self,
        _channel_id: u16,
        seq: u64,
        cols: u16,
        rows: u16,
    ) -> Result<ResizeOutcome, KeeperFault> {
        *held(&self.state) = Some(TerminalState {
            applied_seq: seq,
            cols,
            rows,
        });
        Ok(ResizeOutcome::Applied { seq, cols, rows })
    }
    fn begin_input(&self, _channel_id: u16, bytes: Vec<u8>) -> KeeperInputCommand {
        let written = u32::try_from(bytes.len()).unwrap();
        held(&self.written).extend_from_slice(&bytes);
        let answer = held(&self.answers)
            .pop_front()
            .unwrap_or(KeeperInputResult::Ack { written });
        KeeperInputCommand {
            admission: Ok(()),
            result: Box::pin(std::future::ready(answer)),
        }
    }
    fn write_legacy_input(&self, _channel_id: u16, _bytes: &[u8]) -> Result<(), KeeperFault> {
        Ok(())
    }
}

pub fn case(frame: &ServerFrame) -> &'static str {
    match frame {
        ServerFrame::Ready(_) => "ready",
        ServerFrame::TerminalViewState(_) => "terminalViewState",
        ServerFrame::CellGrid(_) => "cellGrid",
        ServerFrame::CellGridChunk(_) => "cellGridChunk",
        ServerFrame::InputAccepted(_) => "inputAccepted",
        ServerFrame::InputRejected(_) => "inputRejected",
        ServerFrame::InputAmbiguous(_) => "inputAmbiguous",
        ServerFrame::Scrollback(_) => "scrollback",
        ServerFrame::Closed(_) => "closed",
        ServerFrame::InputRouteResult(_) => "inputRouteResult",
        ServerFrame::TransportProbeResult(_) => "transportProbeResult",
    }
}

/// The close reason of a `closed` frame.
pub fn closed_reason(frame: &ServerFrame) -> String {
    match frame {
        ServerFrame::Closed(closed) => closed.reason.clone(),
        other => panic!("expected a closed frame, got {}", case(other)),
    }
}

pub fn digest(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn grant(
    grant_id: &str,
    secret: &str,
    session_ids: &[&str],
    tab_id: &str,
    ttl_ms: u32,
) -> DLocalTerminalGrant {
    DLocalTerminalGrant {
        request_id: format!("install-{grant_id}"),
        grant_id: grant_id.to_owned(),
        secret_sha256: digest(secret),
        session_ids: session_ids.iter().map(|id| (*id).to_owned()).collect(),
        device_fingerprint: DEVICE.to_owned(),
        tab_id: tab_id.to_owned(),
        ttl_ms,
        worker_epoch: WORKER_EPOCH.to_owned(),
        ..Default::default()
    }
}

pub fn hello(grant_id: &str, secret: &str, tab_id: &str) -> ClientFrame {
    ClientFrame::from(LocalTerminalHello {
        grant_id: grant_id.to_owned(),
        secret: secret.to_owned(),
        tab_id: tab_id.to_owned(),
        device_fingerprint: DEVICE.to_owned(),
        ..Default::default()
    })
}

pub fn view(session_id: &str, revision: u64) -> ClientFrame {
    view_with_id(
        &format!("{revision:08x}-0000-4000-8000-000000000000"),
        session_id,
        revision,
    )
}

/// v2's `view()` mints a fresh view id per call; two sockets sharing one id
/// would be one view moving between them.
pub fn view_with_id(view_id: &str, session_id: &str, revision: u64) -> ClientFrame {
    ClientFrame::from(TerminalViewCommand {
        view_id: view_id.to_owned(),
        session_id: session_id.to_owned(),
        cols: 40,
        rows: 12,
        revision,
        active: true,
        ..Default::default()
    })
}

pub fn input(session_id: &str, input_seq: u64, data: &[u8]) -> ClientFrame {
    ClientFrame::from(InputCommand {
        session_id: session_id.to_owned(),
        input_seq,
        data: data.to_vec(),
        ..Default::default()
    })
}

pub fn encode(frame: ClientFrame) -> Vec<u8> {
    LocalTerminalClientFrame {
        frame: Some(frame),
        ..Default::default()
    }
    .encode_to_vec()
}

/// Let spawned closes, writes, applies and cadence passes run.
pub async fn settle() {
    for _ in 0..32 {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// One worker's direct terminal path over the stream harness's session.
pub struct Fixture {
    pub harness: Harness,
    pub keeper: Arc<AutoKeeper>,
    pub view: Arc<TerminalViewOwner>,
    pub door: Arc<LocalTerminalDoor>,
    pub sockets: Arc<LocalTerminalSockets>,
    pub grants: LocalTerminalGrantStore,
    pub routes: TerminalInputRouteOwner,
    upstream: UplinkReceiver,
    cadence: tokio::task::JoinHandle<()>,
    next_socket: AtomicUsize,
}

impl Fixture {
    /// The fixture with v2's grant installed: the live session and a granted
    /// session this worker does not hold.
    pub fn new() -> Self {
        let keeper = Arc::new(AutoKeeper::default());
        let harness = Harness::with_keeper(
            RioCore::new(COLS, ROWS),
            Arc::clone(&keeper) as Arc<dyn KeeperChannels>,
        );
        let (uplink, upstream) = channel();
        let clock: Arc<dyn EventClock> = Arc::new(SystemClock);
        let coord_sink = Arc::new(CoordinatorCellSink::new(Arc::new(ProtoLinkWire)));
        let (cadence, cadence_task) = CellCadence::spawn(
            Arc::clone(&harness.emitter),
            Arc::clone(&harness.table),
            Arc::clone(&clock),
            uplink.clone(),
            coord_sink,
        );
        // v2's harness `"coord"` sink accepts every frame
        // (`terminal-stream-state-harness.ts:92-96`). The cadence just
        // replaced it with a link sink that no link ever attaches, which
        // refuses every baseline part and so holds every delta stream-wide.
        let recording = Arc::clone(&harness.sink) as Arc<dyn CellSink>;
        held(&harness.emitter).register_cell_sink(recording);
        let view = TerminalViewOwner::new(TerminalViewOwnerDeps {
            sessions: Arc::new(SessionViewPort::new(
                Arc::clone(&harness.manager),
                Arc::clone(&harness.table),
                cadence,
            )),
            uplink,
            clock,
            runtime: tokio::runtime::Handle::current(),
        });
        let work_budget = TerminalInputWorkBudget::new();
        let routes = TerminalInputRouteOwner::new(
            WORKER_EPOCH.to_owned(),
            Arc::clone(&harness.table),
            Arc::clone(harness.manager.control_lanes()),
            work_budget.clone(),
        );
        let door = LocalTerminalDoor::new(LocalTerminalDoorDeps {
            manager: Arc::clone(&harness.manager),
            sessions: Arc::clone(&harness.table),
            view: Arc::clone(&view),
            routes: routes.clone(),
            work_budget,
            worker_fingerprint: WORKER_FP.to_owned(),
            process_epoch: WORKER_EPOCH.to_owned(),
            runtime: tokio::runtime::Handle::current(),
        });
        let sockets = door.sockets();
        let grants = door.grants();
        grants
            .install(&grant(
                GRANT_ID,
                SECRET,
                &[SESSION, GRANTED_DEAD_SESSION],
                TAB,
                60_000,
            ))
            .unwrap();
        Self {
            harness,
            keeper,
            view,
            door,
            sockets,
            grants,
            routes,
            upstream,
            cadence: cadence_task,
            next_socket: AtomicUsize::new(0),
        }
    }

    /// A loopback carrier that just opened.
    pub fn open(&self) -> Arc<StubPort> {
        let ordinal = self.next_socket.fetch_add(1, Ordering::SeqCst) + 1;
        StubPort::open_on(
            &self.sockets,
            format!("{ordinal:08x}-5000-4000-8000-000000000000"),
        )
    }

    pub fn send(&self, port: &StubPort, frame: ClientFrame) {
        self.sockets.on_message(port, &encode(frame));
    }

    /// Whether the emitter still delivers to this socket's cell sink.
    pub fn has_sink(&self, port: &StubPort) -> bool {
        held(&self.harness.emitter)
            .sinks()
            .contains(&local_cell_sink_id(port.socket_id()))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.door.dispose();
        self.view.dispose();
        self.cadence.abort();
        while self.upstream.try_recv().is_some() {}
    }
}
