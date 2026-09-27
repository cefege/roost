//! The fixture a spawn is driven through: a keeper that opens or refuses PTYs,
//! and a durable boundary that records every claim.
//!
//! It is here rather than in the test file because a spawn's fixtures are the
//! whole shape of the thing under test — two claims, a PTY, an `opened` event —
//! and inlined they were most of the file. Every variant these tests assert is
//! about a resource that leaks, and reading them needs the ledger to be in
//! sight without scrolling past it first.

#![allow(clippy::unwrap_used, clippy::expect_used)]
// `std::sync::Mutex` and not `parking_lot`, deliberately. A parking_lot lock has
// no poisoning at all, and the recovery this crate relies on is the point: a
// panic while a lock was held says nothing about whether what it guarded is
// still readable, and in a worker a panicking lock is fleet-visible. Every lock
// in this crate recovers with `unwrap_or_else(|poisoned| poisoned.into_inner())`
// rather than merely unwrapping, so the standard lock is the one that keeps the
// behaviour and `parking_lot` is not a dependency here.

#[path = "../session_emit_support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex};

use roost_protocol::wire::event::SessionEvent;
use roost_worker::event_store::{DurableEventKind, Reservation, Store};
use roost_worker::session::sinks::{ChannelBinding, SessionEventError, SessionEventSink};
use roost_worker::session::spawn::{ShellSpawner, ShellSpecResolver, SpawnContext, SpawnRequest};

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

pub struct LedgerSink {
    store: Mutex<Store>,
    ledger: Mutex<ClaimLedger>,
}

impl LedgerSink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            store: Mutex::new(Store::new()),
            ledger: Mutex::new(ClaimLedger::default()),
        })
    }

    pub fn reserve(&self, kind: DurableEventKind) -> Reservation {
        self.store
            .lock()
            .unwrap()
            .reserve_default(kind)
            .expect("a fresh store has room")
    }

    pub fn released(&self) -> Vec<u64> {
        self.ledger.lock().unwrap().released.clone()
    }

    pub fn held(&self) -> Vec<u64> {
        self.ledger.lock().unwrap().held.clone()
    }

    pub fn emitted(&self) -> Vec<Option<SessionEvent>> {
        self.ledger
            .lock()
            .unwrap()
            .emitted
            .iter()
            .map(|(_, event)| event.clone())
            .collect()
    }

    pub fn live_claims(&self) -> usize {
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
pub struct FakeKeeper {
    refuse: bool,
    opened: Mutex<Vec<(i64, u16, u16)>>,
    killed: Mutex<Vec<i64>>,
}

impl FakeKeeper {
    pub fn refusing() -> Arc<Self> {
        Arc::new(Self {
            refuse: true,
            opened: Mutex::new(Vec::new()),
            killed: Mutex::new(Vec::new()),
        })
    }

    pub fn working() -> Arc<Self> {
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

pub struct FixedResolver {
    cwd: String,
}

impl ShellSpecResolver for FixedResolver {
    fn resolve_shell_spec(&self, _cwd: &str, _session_id: &str) -> Result<ShellSpec, String> {
        Ok(shell_spec(&self.cwd))
    }
}

pub struct BindingThatRecordsDelivery;

impl ChannelBinding for BindingThatRecordsDelivery {
    fn on_output(&self, _chunk: &[u8]) {}
    fn on_exit(&self, _exit_code: Option<i32>) {}
    fn on_error(&self, _reason: String) {}
}

pub fn request(channel_id: i64) -> SpawnRequest {
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

pub fn context<'a>(
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
