//! The spawn: the two durable claims, the PTY, and the record. Every variant
//! here is about a resource that leaks — a claim the store can never give back,
//! a PTY nobody can reach, or an `opened` event a browser learns about twice.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "session_emit_support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex};

use roost_protocol::wire::event::SessionEvent;
use roost_worker::channel_fsm::ChannelState;
use roost_worker::event_store::{DurableEventKind, Reservation, Store};
use roost_worker::session::ids::mint_uuid;
use roost_worker::session::sinks::{ChannelBinding, SessionEventError, SessionEventSink};
use roost_worker::session::spawn::{
    ShellSpawner, ShellSpecResolver, SpawnContext, SpawnRefusal, SpawnRequest,
    canonical_session_cwd, spawn_shell,
};
use roost_worker::session::types::SessionRecord;
use roost_worker::shell_spec::ShellSpec;

use support::{channel, shell_spec, worker_fp};

/// What the durable boundary did, as a ledger of claim ids and events.
#[derive(Debug, Default)]
struct ClaimLedger {
    emitted: Vec<(u64, Option<SessionEvent>)>,
    held: Vec<u64>,
    released: Vec<u64>,
    /// When set, every emit fails with this.
    refuse_emit: bool,
}

struct LedgerSink {
    store: Mutex<Store>,
    ledger: Mutex<ClaimLedger>,
}

impl LedgerSink {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            store: Mutex::new(Store::new()),
            ledger: Mutex::new(ClaimLedger::default()),
        })
    }

    fn reserve(&self, kind: DurableEventKind) -> Reservation {
        self.store
            .lock()
            .unwrap()
            .reserve_default(kind)
            .expect("a fresh store has room")
    }

    fn released(&self) -> Vec<u64> {
        self.ledger.lock().unwrap().released.clone()
    }

    fn held(&self) -> Vec<u64> {
        self.ledger.lock().unwrap().held.clone()
    }

    fn emitted(&self) -> Vec<Option<SessionEvent>> {
        self.ledger
            .lock()
            .unwrap()
            .emitted
            .iter()
            .map(|(_, event)| event.clone())
            .collect()
    }

    fn live_claims(&self) -> usize {
        self.store.lock().unwrap().live_reservations()
    }
}

impl SessionEventSink for LedgerSink {
    fn reserve(&self, kind: DurableEventKind) -> Result<Reservation, SessionEventError> {
        self.store
            .lock()
            .unwrap()
            .reserve(kind, kind.payload_limit())
            .map_err(SessionEventError::from)
    }

    fn hold(&self, reservation: Reservation) {
        self.ledger.lock().unwrap().held.push(reservation.id());
    }

    fn release(&self, reservation: Reservation) {
        self.ledger.lock().unwrap().released.push(reservation.id());
    }

    fn emit(
        &self,
        event: &SessionEvent,
        reservation: Option<Reservation>,
    ) -> Result<(), SessionEventError> {
        let mut ledger = self.ledger.lock().unwrap();
        if ledger.refuse_emit {
            return Err(SessionEventError::Unclassifiable(
                "the store is full".to_owned(),
            ));
        }
        ledger.emitted.push((
            reservation.map(Reservation::id).unwrap_or(0),
            Some(event.clone()),
        ));
        Ok(())
    }
}

/// A keeper that opens PTYs, and remembers what it was asked for.
struct FakeKeeper {
    refuse: bool,
    opened: Mutex<Vec<(i64, u16, u16)>>,
    killed: Mutex<Vec<i64>>,
}

impl FakeKeeper {
    fn refusing() -> Arc<Self> {
        Arc::new(Self {
            refuse: true,
            opened: Mutex::new(Vec::new()),
            killed: Mutex::new(Vec::new()),
        })
    }

    fn working() -> Arc<Self> {
        Arc::new(Self {
            refuse: false,
            opened: Mutex::new(Vec::new()),
            killed: Mutex::new(Vec::new()),
        })
    }
}

impl ShellSpawner for FakeKeeper {
    fn spawn_channel(
        &self,
        channel_id: roost_protocol::wire::brand::ChannelId,
        _spec: &ShellSpec,
        cols: u16,
        rows: u16,
        _binding: Arc<dyn ChannelBinding>,
    ) -> Result<u32, String> {
        if self.refuse {
            return Err("channel_id in use".to_owned());
        }
        self.opened
            .lock()
            .unwrap()
            .push((channel_id.as_u32() as i64, cols, rows));
        Ok(4242)
    }

    fn kill_channel(&self, channel_id: roost_protocol::wire::brand::ChannelId) {
        self.killed.lock().unwrap().push(channel_id.as_u32() as i64);
    }
}

struct FixedResolver {
    cwd: String,
}

impl ShellSpecResolver for FixedResolver {
    fn resolve_shell_spec(&self, _cwd: &str, _session_id: &str) -> Result<ShellSpec, String> {
        Ok(shell_spec(&self.cwd))
    }
}

struct BindingThatRecordsDelivery;

impl ChannelBinding for BindingThatRecordsDelivery {
    fn on_output(&self, _chunk: &[u8]) {}
    fn on_exit(&self, _exit_code: Option<i32>) {}
    fn on_error(&self, _reason: String) {}
}

fn request(channel_id: i64) -> SpawnRequest {
    SpawnRequest {
        channel_id: channel(channel_id),
        folder: "/tmp".to_owned(),
        cols: 0,
        rows: 0,
        session_id: None,
        shell_spec: None,
        event: DurableEventKind::Opened,
    }
}

fn context<'a>(
    keeper: &'a Arc<FakeKeeper>,
    events: &'a Arc<LedgerSink>,
    resolver: &'a FixedResolver,
) -> SpawnContext<'a> {
    SpawnContext {
        spawner: keeper.as_ref(),
        resolver,
        events: events.as_ref(),
        worker_fp: &worker_fp(),
    }
}

/// A spawn that cannot open its PTY must give BOTH claims back. A leaked claim
/// is capacity the store will never hand out again, and a store that has lost
/// its capacity refuses every later write in the worker, not just this one.
#[test]
fn a_spawn_the_keeper_refuses_releases_both_claims_and_opens_nothing() {
    let events = LedgerSink::new();
    let opened = events.reserve(DurableEventKind::Opened);
    let close = events.reserve(DurableEventKind::Closed);
    let keeper = FakeKeeper::refusing();
    let resolver = FixedResolver {
        cwd: "/tmp".to_owned(),
    };

    let refused = spawn_shell(
        &context(&keeper, &events, &resolver),
        opened,
        close,
        Arc::new(BindingThatRecordsDelivery),
        request(21),
        1_000,
    );
    assert!(
        matches!(refused, Err(SpawnRefusal::KeeperRefused { .. })),
        "the refusal did not surface, got {refused:?}"
    );
    let mut released = events.released();
    released.sort_unstable();
    let mut expected = vec![opened.id(), close.id()];
    expected.sort_unstable();
    assert_eq!(released, expected, "a claim survived the failed spawn");
    assert!(
        events.emitted().is_empty(),
        "a refused spawn announced a session"
    );
    assert!(keeper.opened.lock().unwrap().is_empty());
    assert_eq!(
        events.live_claims(),
        0,
        "the store is still holding capacity"
    );
}

/// The successful shape: the `opened` claim is consumed by the event, the
/// close claim is committed and left ON THE RECORD for the close to consume,
/// and the record comes back attached.
#[test]
fn a_spawn_consumes_the_opened_claim_and_leaves_the_close_claim_committed() {
    let events = LedgerSink::new();
    let opened = events.reserve(DurableEventKind::Opened);
    let close = events.reserve(DurableEventKind::Closed);
    let keeper = FakeKeeper::working();
    let resolver = FixedResolver {
        cwd: "/tmp".to_owned(),
    };

    let record: SessionRecord = spawn_shell(
        &context(&keeper, &events, &resolver),
        opened,
        close,
        Arc::new(BindingThatRecordsDelivery),
        request(22),
        1_000,
    )
    .expect("a working keeper spawns");

    let emitted = events.emitted();
    assert_eq!(emitted.len(), 1, "the spawn announced more than one event");
    match emitted[0].as_ref().expect("an event was written") {
        SessionEvent::Opened {
            session_id,
            channel,
            cwd,
            ts,
            ..
        } => {
            assert_eq!(session_id, record.session_id());
            assert_eq!(*channel, record.channel_id());
            assert_eq!(cwd, &record.identity.cwd);
            assert_eq!(*ts, 1_000);
        }
        other => panic!("a spawn announced {other:?} rather than an opened"),
    }
    assert_eq!(
        events.held(),
        vec![close.id()],
        "the close claim was not committed"
    );
    assert!(
        events.released().is_empty(),
        "a successful spawn released a claim"
    );
    assert_eq!(
        record.fsm.state(),
        Some(ChannelState::Attached),
        "a spawned channel is not attached, so nothing may ever close it"
    );
    assert_eq!(record.child_pid, Some(4242));
    assert_eq!(keeper.opened.lock().unwrap().as_slice(), &[(22, 80, 24)]);
    assert_eq!(record.identity.socket_path, "mux:22");
}

/// A respawn announces a `respawned`, not an `opened`. An `opened` here would
/// tell every browser watching that row to paint a start moment it never had.
#[test]
fn a_respawn_announces_a_respawn_and_not_an_opened() {
    let events = LedgerSink::new();
    let opened = events.reserve(DurableEventKind::State);
    let close = events.reserve(DurableEventKind::Closed);
    let keeper = FakeKeeper::working();
    let resolver = FixedResolver {
        cwd: "/tmp".to_owned(),
    };
    let mut wanted = request(23);
    wanted.event = DurableEventKind::State;
    wanted.session_id = Some(support::session_id());
    wanted.shell_spec = Some(shell_spec("/somewhere/that/is/gone"));

    let record = spawn_shell(
        &context(&keeper, &events, &resolver),
        opened,
        close,
        Arc::new(BindingThatRecordsDelivery),
        wanted,
        2_000,
    )
    .expect("a respawn succeeds");

    match events.emitted()[0].as_ref().expect("an event was written") {
        SessionEvent::Respawned {
            session_id,
            new_channel,
            ..
        } => {
            assert_eq!(
                session_id,
                &support::session_id(),
                "the respawn changed the session id"
            );
            assert_eq!(*new_channel, record.channel_id());
        }
        other => panic!("a respawn announced {other:?}"),
    }
    // The retained launch contract is used verbatim: a folder that has since
    // been deleted must not fail a session that was working a second ago.
    assert_eq!(record.identity.shell_spec.cwd, "/somewhere/that/is/gone");
}

/// Geometry is validated before anything is claimed or opened, and a request
/// that states none gets the default rather than a zero-sized PTY.
#[test]
fn geometry_is_refused_before_a_pty_or_a_claim_is_touched() {
    let events = LedgerSink::new();
    let opened = events.reserve(DurableEventKind::Opened);
    let close = events.reserve(DurableEventKind::Closed);
    let keeper = FakeKeeper::working();
    let resolver = FixedResolver {
        cwd: "/tmp".to_owned(),
    };
    let mut too_wide = request(24);
    too_wide.cols = 900;

    let refused = spawn_shell(
        &context(&keeper, &events, &resolver),
        opened,
        close,
        Arc::new(BindingThatRecordsDelivery),
        too_wide,
        1_000,
    );
    assert!(
        matches!(refused, Err(SpawnRefusal::Geometry { cols: 900, .. })),
        "an impossible geometry was not refused, got {refused:?}"
    );
    assert!(
        events.released().is_empty(),
        "a refused geometry released a claim"
    );
    assert!(events.emitted().is_empty());
    assert!(keeper.opened.lock().unwrap().is_empty());
}

/// An event kind that is neither a spawn nor a respawn has no business
/// announcing a PTY, and it is refused rather than coerced into one.
#[test]
fn a_close_may_not_announce_a_spawn() {
    let events = LedgerSink::new();
    let opened = events.reserve(DurableEventKind::Closed);
    let close = events.reserve(DurableEventKind::Closed);
    let keeper = FakeKeeper::working();
    let resolver = FixedResolver {
        cwd: "/tmp".to_owned(),
    };
    let mut wrong = request(25);
    wrong.event = DurableEventKind::Exited;

    let refused = spawn_shell(
        &context(&keeper, &events, &resolver),
        opened,
        close,
        Arc::new(BindingThatRecordsDelivery),
        wrong,
        1_000,
    );
    assert!(
        matches!(refused, Err(SpawnRefusal::UnnameableEvent { .. })),
        "an unnameable event was not refused, got {refused:?}"
    );
    assert!(keeper.opened.lock().unwrap().is_empty());
}
