//! The fixtures the two foreground-liveness suites share: a client whose pane is
//! open and whose stream the authority has named, and the readers that answer
//! "what does this replica owe, and what went out".
//!
//! Owned here because both suites assert against the SAME identities (a pane, a
//! stream, a checkpoint) and the SAME instants. Time is passed explicitly in
//! every helper: this crate has no timer, so a sweep at a chosen `now_ms` is
//! the only way to place a deadline, and a wall clock would make every case a
//! race.
//!
//! The link is built here rather than borrowed from `sync_decode_support`
//! because that module re-includes `support/hydration.rs` under its own path,
//! and loading it beside `support` compiles the same file twice — which makes
//! the shared `ClientCore` helpers two distinct types and fails clippy before it
//! runs a single lint. `support::hydration::answer_hydrations` is the owner of
//! the hydration answer and is the one thing used from there.
#![allow(dead_code, unused_imports, clippy::unwrap_used, clippy::expect_used)]

#[path = "../support/mod.rs"]
mod support;

use roost_client_core::effect::{Effect, SyncCommand};
use roost_client_core::event::ClientEvent;
use roost_client_core::sync::decode::{SyncFrameMeta, decode_firehose};
use roost_client_core::terminal::liveness::RepairOutcome;
use roost_client_core::{ClientCore, SyncDomain, TerminalSession, TerminalToken};
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::buffa::Message;
use roost_proto::{
    FirehoseFrame, PbCellGridFrame, SyncDomainGeneration, SyncSubscribedFrame,
    TerminalViewStateFrame, TerminalViewStatus,
};

pub use support::hydration;
pub use support::{
    EPOCH, OTHER_STREAM, SESSION, STREAM, delta, full, replica_with_baseline, sync_token,
};

/// The pane's own identity, stable for the life of the pane.
pub const VIEW: &str = "00000000-0000-4000-8000-0000000000c1";
/// The pane's effective rows, which the authority also mints the stream at.
pub const ROWS: u32 = 4;
/// The interval a healthy foreground pane is given between its last frame and
/// the probe. Restated here because every case reasons in terms of it.
pub const IDLE_PROBE_MS: u64 = 5_000;
/// The interval a published challenge is given to be answered.
pub const PROOF_DEADLINE_MS: u64 = 3_000;

const TAB: &str = "tab-liveness";
const SOCKET: &str = "sock-liveness";
const PROCESS_EPOCH: &str = "epoch-liveness";
const DOMAIN_GENERATION: u64 = 3;

pub const WORKER_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn wire_domain(domain: SyncDomain) -> roost_proto::SyncDomain {
    match domain {
        SyncDomain::Terminal => roost_proto::SyncDomain::Terminal,
        SyncDomain::Workers => roost_proto::SyncDomain::Workers,
        SyncDomain::Workspaces => roost_proto::SyncDomain::Workspaces,
        SyncDomain::Tasks => roost_proto::SyncDomain::Tasks,
        SyncDomain::Mcp => roost_proto::SyncDomain::Mcp,
        SyncDomain::Pair => roost_proto::SyncDomain::Pair,
        SyncDomain::Audit => roost_proto::SyncDomain::Audit,
    }
}

/// A control frame, stamped as the coordinator stamps a control arm.
fn control(arm: Frame) -> Vec<u8> {
    FirehoseFrame {
        delivery_seq: 0,
        domain: roost_proto::SyncDomain::Unspecified.into(),
        domain_generation: 0,
        frame: Some(arm),
        ..FirehoseFrame::default()
    }
    .encode_to_vec()
}

/// An application frame, stamped as `retained_frame::stamp_envelope` stamps it.
pub fn application(domain: SyncDomain, delivery_seq: u64, arm: Frame) -> Vec<u8> {
    FirehoseFrame {
        delivery_seq,
        domain: wire_domain(domain).into(),
        domain_generation: DOMAIN_GENERATION,
        frame: Some(arm),
        ..FirehoseFrame::default()
    }
    .encode_to_vec()
}

/// The `subscribed` barrier the coordinator sends: every domain, one generation,
/// subscribed.
fn subscribed_arm() -> Frame {
    Frame::Subscribed(Box::new(SyncSubscribedFrame {
        socket_id: SOCKET.to_owned(),
        process_epoch: PROCESS_EPOCH.to_owned(),
        generations: SyncDomain::ALL
            .into_iter()
            .map(|domain| SyncDomainGeneration {
                domain: wire_domain(domain).into(),
                generation: DOMAIN_GENERATION,
                subscribed: true,
                ..SyncDomainGeneration::default()
            })
            .collect(),
        ..SyncSubscribedFrame::default()
    }))
}

/// Decode bytes delivered on `generation`.
fn decoded(bytes: &[u8], generation: u64) -> ClientEvent {
    decode_firehose(bytes, SyncFrameMeta { generation }).expect("the fixture's own frame decodes")
}

/// A client whose link has every domain subscribed, hydrated and ready.
pub fn ready_core() -> (ClientCore, u64) {
    let mut core = ClientCore::in_memory(TAB);
    let generation = open_ready_link(&mut core);
    (core, generation)
}

/// Dial, open, decode the coordinator's `subscribed`, and answer hydration.
/// Returns the socket generation, so a second call is a rotation.
pub fn open_ready_link(core: &mut ClientCore) -> u64 {
    let generation = match core.handle(ClientEvent::DialRequested).as_slice() {
        [Effect::DialSync { generation, .. }] => *generation,
        other => panic!("expected exactly one dial, got {other:?}"),
    };
    core.handle(ClientEvent::SyncLinkOpened {
        generation,
        socket_id: SOCKET.to_owned(),
        process_epoch: PROCESS_EPOCH.to_owned(),
    });
    let effects = core.handle(decoded(&control(subscribed_arm()), generation));
    hydration::answer_hydrations(core, &effects);
    generation
}

/// Decode on `generation` and apply.
pub fn deliver(core: &mut ClientCore, generation: u64, bytes: &[u8]) -> Vec<Effect> {
    core.handle(decoded(bytes, generation))
}

/// The accepted answer that installs the stream both suites fold into.
fn accepted_view_state() -> Frame {
    Frame::TerminalViewState(Box::new(TerminalViewStateFrame {
        view_id: VIEW.to_owned(),
        session_id: SESSION.to_owned(),
        status: TerminalViewStatus::Accepted.into(),
        stream_id: STREAM.to_owned(),
        effective_cols: 8,
        effective_rows: ROWS,
        ..TerminalViewStateFrame::default()
    }))
}

/// The generation this client is fenced to.
pub fn token(core: &ClientCore) -> TerminalToken {
    core.store()
        .sync_terminal_token()
        .expect("a ready link has a terminal token")
}

/// A client with one pane open and its stream installed, and NO baseline: the
/// shape a dropped seed leaves behind.
pub fn pane_without_baseline() -> (ClientCore, u64) {
    let (mut core, generation) = ready_core();
    open_pane(&mut core, generation);
    (core, generation)
}

/// A client with one pane open, its stream installed, and one accepted baseline,
/// so the probe is armed at `now_ms` exactly as it is in a live document.
pub fn painted(now_ms: u64) -> (ClientCore, u64) {
    let (mut core, generation) = ready_core();
    open_pane(&mut core, generation);
    let token = token(&core);
    let replica = replica_of(&mut core);
    assert_eq!(
        replica.admit_frame(&full(ROWS), false, &token, now_ms),
        roost_client_core::Admission::BaselineReplaced,
        "the fixture's own baseline must be admissible"
    );
    (core, generation)
}

/// Open the pane and deliver the authority's acceptance of it.
fn open_pane(core: &mut ClientCore, generation: u64) {
    core.handle(ClientEvent::ViewOpened {
        session_id: SESSION.to_owned(),
        worker_fp: WORKER_FP.to_owned(),
        view_id: VIEW.to_owned(),
        cols: 8,
        rows: ROWS,
    });
    deliver(
        core,
        generation,
        &application(SyncDomain::Terminal, 1, accepted_view_state()),
    );
}

pub fn replica_of(core: &mut ClientCore) -> &mut TerminalSession {
    core.store_mut()
        .terminal_mut_if_present(SESSION)
        .expect("the pane created a replica")
}

/// Admit one frame on the replica's own generation, which is the only admission
/// that notes liveness at all.
pub fn admit(
    core: &mut ClientCore,
    frame: &PbCellGridFrame,
    now_ms: u64,
) -> roost_client_core::Admission {
    let token = token(core);
    replica_of(core).admit_frame(frame, false, &token, now_ms)
}

/// The probe deadline, the proof deadline, and the outstanding challenge.
pub fn deadlines(core: &ClientCore) -> (Option<u64>, Option<u64>, Option<u64>) {
    let liveness = core
        .store()
        .terminal(SESSION)
        .expect("the pane created a replica")
        .liveness();
    (
        liveness.quiet_due_ms(),
        liveness.proof_due_ms(),
        liveness.challenged_at_ms(),
    )
}

pub fn repair_attempts(core: &ClientCore) -> u32 {
    core.store()
        .terminal(SESSION)
        .expect("the pane created a replica")
        .liveness()
        .repair_attempts()
}

pub fn outcome(core: &ClientCore) -> RepairOutcome {
    core.store()
        .terminal(SESSION)
        .expect("the pane created a replica")
        .liveness()
        .outcome()
}

pub fn sweep(core: &mut ClientCore, now_ms: u64) -> Vec<Effect> {
    core.handle(ClientEvent::Sweep { now_ms })
}

/// Every scoped resync among `effects`. Anything else the sweep emits on the way
/// — a view heartbeat, a cursor write — is a different deadline.
pub fn challenges(effects: &[Effect]) -> Vec<&SyncCommand> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::SendSync(command) => match command {
                SyncCommand::TerminalResync { .. } => Some(command),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

pub fn link_closes(effects: &[Effect]) -> Vec<(u64, String)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::CloseSyncLink { generation, reason } => Some((*generation, reason.clone())),
            _ => None,
        })
        .collect()
}

/// Arm BOTH deadlines, which is the state a frame arriving mid-challenge
/// produces: the proof still owes an answer and the probe re-anchored.
pub fn arm_both(core: &mut ClientCore, challenge_ms: u64, probe_ms: u64) {
    let token = token(core);
    let replica = replica_of(core);
    replica.begin_scoped_repair(&token, challenge_ms);
    replica.arm_quiet_probe(&token, probe_ms);
    assert_eq!(
        deadlines(core),
        (
            Some(probe_ms + IDLE_PROBE_MS),
            Some(challenge_ms + PROOF_DEADLINE_MS),
            Some(challenge_ms)
        ),
        "the fixture must arm both deadlines, or the retirements it feeds prove nothing"
    );
}
