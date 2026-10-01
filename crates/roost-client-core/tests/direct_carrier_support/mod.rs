//! The fixtures the three direct-carrier suites share: one carrier for
//! `worker-a`, a client whose Sync terminal domain is ready and which is watching
//! a pane, and the three shapes of answer this chain produces — a published view,
//! an elected route, and a retired one.
//!
//! Owned here because all three suites assert against the SAME identities (a
//! pane, a minted id, a stream), and a second copy of those constants is a second
//! answer to "which id is the worker's" arriving from a different file.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

#[path = "../support/mod.rs"]
mod support;

pub use roost_client_core::{
    ClientCore, ClientEvent, DirectCarrier, DirectCommand, Effect, SyncCommand, SyncFrame,
    TerminalToken, TerminalTransport, ViewIdTarget, ViewIntent,
};
pub use roost_proto::PbCellGridFrame;
pub use support::sync_reconnect::open_ready_link;
pub use support::{EPOCH, SESSION, STREAM, client, row};

pub const WORKER: &str = "worker-a";
/// The pane's own identity, which never changes.
pub const VIEW: &str = "11111111-1111-4111-8111-111111111111";
/// A second session, for the grant-scope and collision cases.
pub const OTHER_SESSION: &str = "00000000-0000-4000-8000-00000000000b";
pub const OTHER_VIEW: &str = "33333333-3333-4333-8333-333333333333";
/// The id the host mints for the candidate's own view.
pub const WIRE: &str = "22222222-2222-4222-8222-222222222222";
/// A second minted id, for the collision case.
pub const OTHER_WIRE: &str = "44444444-4444-4444-8444-444444444444";
pub const COLS: u32 = 80;
pub const ROWS: u32 = 24;

/// The generation the loopback socket presents, and therefore the one a direct
/// view-state is stamped with.
pub const SOCKET_GENERATION: u64 = 1;
pub const PROCESS_EPOCH: &str = "epoch-a";
pub const DOMAIN_GENERATION: u64 = 7;

/// The token a loopback carrier for `worker-a` presents.
pub fn direct_token() -> TerminalToken {
    TerminalToken::direct(
        SOCKET_GENERATION,
        TerminalTransport::Loopback,
        WORKER,
        PROCESS_EPOCH,
        DOMAIN_GENERATION,
    )
}

/// A loopback carrier for `worker-a`, admitting exactly `sessions`.
pub fn carrier(sessions: &[&str]) -> DirectCarrier {
    DirectCarrier {
        connection_id: "loopback-a".to_owned(),
        worker_fp: WORKER.to_owned(),
        transport: TerminalTransport::Loopback,
        token: direct_token(),
        granted_sessions: sessions.iter().map(|id| (*id).to_owned()).collect(),
    }
}

/// A client whose Sync terminal domain is ready and which is watching one pane.
pub fn core_with_a_pane() -> ClientCore {
    let mut core = client();
    open_ready_link(&mut core, "socket-a");
    let _ = core.handle(viewing(SESSION, VIEW, COLS, ROWS));
    core
}

pub fn viewing(session_id: &str, view_id: &str, cols: u32, rows: u32) -> ClientEvent {
    ClientEvent::ViewOpened {
        session_id: session_id.to_owned(),
        worker_fp: WORKER.to_owned(),
        view_id: view_id.to_owned(),
        cols,
        rows,
    }
}

/// The attempt id one `MintTerminalViewId` effect names.
pub fn mint_attempt(effects: &[Effect]) -> u64 {
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::MintTerminalViewId { attempt_id, .. } => Some(*attempt_id),
            _ => None,
        })
        .expect("the staged attempt asks the host for an id")
}

/// Every `(session, view)` these effects publish onto a direct carrier.
pub fn direct_publishes(effects: &[Effect]) -> Vec<(String, String)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::SendDirect {
                command:
                    DirectCommand::View {
                        session_id,
                        view_id,
                        ..
                    },
                ..
            } => Some((session_id.clone(), view_id.clone())),
            _ => None,
        })
        .collect()
}

/// Every `(session, view, intent)` these effects send on the Sync socket.
pub fn sync_view_intents(effects: &[Effect]) -> Vec<(String, String, ViewIntent)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::SendSync(SyncCommand::TerminalView {
                session_id,
                view_id,
                intent,
                ..
            }) => Some((session_id.clone(), view_id.clone(), *intent)),
            _ => None,
        })
        .collect()
}

/// The host's answer to a mint request, with the id it chose.
pub fn minted(core: &mut ClientCore, attempt_id: u64, wire_view_id: Option<&str>) -> Vec<Effect> {
    core.handle(ClientEvent::TerminalViewIdMinted {
        session_id: SESSION.to_owned(),
        attempt_id,
        logical_view_id: VIEW.to_owned(),
        target: ViewIdTarget::Candidate,
        wire_view_id: wire_view_id.map(str::to_owned),
    })
}

/// The worker's acceptance of the candidate's own view, naming the stream.
pub fn direct_frame(frame: SyncFrame) -> ClientEvent {
    ClientEvent::DirectFrameReceived {
        token: direct_token(),
        frame,
    }
}

pub fn accepted_view_state() -> SyncFrame {
    SyncFrame::ViewState {
        session_id: SESSION.to_owned(),
        view_id: WIRE.to_owned(),
        generation: SOCKET_GENERATION,
        accepted: true,
        stream_id: STREAM.to_owned(),
        effective_cols: COLS,
        effective_rows: ROWS,
    }
}

/// A complete authoritative full for the stream the answer named.
pub fn baseline() -> SyncFrame {
    SyncFrame::CellGrid {
        session_id: SESSION.to_owned(),
        frame: PbCellGridFrame {
            session_id: SESSION.to_owned(),
            stream_id: STREAM.to_owned(),
            grid_epoch: EPOCH.to_owned(),
            cols: COLS,
            rows: ROWS,
            full: true,
            viewport_rows: (0..ROWS)
                .map(|index| row(index, &format!("r{index}")))
                .collect(),
            seq: 1,
            ..Default::default()
        },
    }
}

/// The first delta AFTER the baseline: same stream, same epoch, one changed row.
///
/// What a promoted route has to keep admitting, and the one frame a fold that
/// only knows about staged candidates cannot place.
pub fn continuation() -> SyncFrame {
    SyncFrame::CellGrid {
        session_id: SESSION.to_owned(),
        frame: PbCellGridFrame {
            session_id: SESSION.to_owned(),
            stream_id: STREAM.to_owned(),
            grid_epoch: EPOCH.to_owned(),
            cols: COLS,
            rows: ROWS,
            full: false,
            viewport_rows: vec![row(0, "changed")],
            base_seq: 1,
            seq: 2,
            ..Default::default()
        },
    }
}
