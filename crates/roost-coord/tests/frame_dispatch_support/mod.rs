//! A BOOTED coordinator with one enrolled worker and one claimed socket, and a
//! dispatcher over that socket.
//!
//! Two things this fixture exists to be honest about. First, `CoordServices`:
//! the dispatcher reads the tenancy scope out of the boot facts to scope every
//! append, and `CoordServices::new` deliberately carries none — a fixture that
//! used it would exercise the "no tenancy scope" close rather than the append.
//! Second, the `LiveEffects`: the deferred reap is a production READ of what an
//! append returns, so a fixture that let the append kill the orphans itself
//! would prove the reader exists while testing nothing about it.
//!
//! `unwrap` and `expect` are denied outside `#[cfg(test)]`, and a shared test
//! fixture is its own crate rather than a module of one, so the exemption has to
//! be stated here. Every panic below names a value the fixture just built.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Barrier, Mutex, PoisonError};

use roost_coord::coord_core::boot_facts::BootFacts;
use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_coord::db::CoordDb;
use roost_coord::events::append::LiveEffects;
use roost_coord::events::event_log::EventLog;
use roost_coord::services::CoordServices;
use roost_coord::worker_link::client_seq::ClientSeqCursor;
use roost_coord::worker_link::dispatch::{FrameClass, InboundFrame};
use roost_coord::worker_link::frame_dispatch::WorkerFrameDispatcher;
use roost_coord::workers::registry::claim_generation;

pub mod events;

use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};

use sqlx::AssertSqlSafe;

use roost_protocol::wire::{
    ChannelId, Session, SessionEvent, SessionId, SessionKind, SessionStatus, WorkerFp,
};

use crate::workers_support::RecordingSocket;

/// The worker every test drives: 64 hex characters, which is what the brand
/// accepts and therefore what a redeemed token would have minted.
pub const WORKER_FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A second machine, for the frames that must not act on this socket's state.
pub const OTHER_FP: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

/// The session every snapshot test opens and then omits.
pub const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";

/// One test's coordinator, its enrolled worker, and its claimed socket.
pub struct LinkFixture {
    /// The process state the dispatcher is built over.
    pub services: Arc<CoordServices>,
    /// The database, for the rows an assertion reads directly.
    pub database: CoordDb,
    /// What the socket was handed, in order.
    pub socket: Arc<RecordingSocket>,
    /// This socket's generation.
    pub handle: Arc<WorkerHandle>,
    root: PathBuf,
}

impl LinkFixture {
    /// A booted coordinator, the worker enrolled, and a socket that has claimed
    /// its generation but has NOT crossed the snapshot barrier.
    pub async fn new(label: &str) -> Self {
        Self::build(label, None).await
    }

    /// The same, over a `LiveEffects` the test supplies, so the append can be
    /// observed from inside itself.
    pub async fn with_effects(label: &str, effects: Arc<dyn LiveEffects>) -> Self {
        Self::build(label, Some(effects)).await
    }

    async fn build(label: &str, effects: Option<Arc<dyn LiveEffects>>) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-link-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated database");
        let tenant =
            roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 1_000)
                .await
                .expect("the self-hosted tenant");
        let mut services = CoordServices::booted(
            database.clone(),
            BootFacts {
                tenant: Some(tenant),
                ..BootFacts::default()
            },
        );
        if let Some(effects) = effects {
            // The one `EventLog`, re-bound to the recorder. Same database, same
            // buses, same publication store -- only the post-commit half moves.
            services.event_log = EventLog::new(
                services.db.clone(),
                Arc::clone(&services.buses),
                Arc::clone(&services.pending_publications),
                effects,
            );
        }
        let services = Arc::new(services);
        sqlx::query(AssertSqlSafe(enroll_sql(WORKER_FP)))
            .execute(database.pool())
            .await
            .expect("the worker row");
        let socket = Arc::new(RecordingSocket::new());
        let handle = Arc::new(WorkerHandle::new(
            worker(WORKER_FP),
            None,
            "generation-1".to_owned(),
            std::collections::BTreeSet::new(),
            socket.sender(),
        ));
        claim_generation(&services.buses, &services.workers, Arc::clone(&handle));
        Self {
            services,
            database,
            socket,
            handle,
            root,
        }
    }

    /// A dispatcher over this socket, from the production accessor.
    #[must_use]
    pub fn dispatcher(&self) -> WorkerFrameDispatcher {
        self.services.worker_dispatcher(Arc::clone(&self.handle))
    }

    /// This worker's `client_seq` cursor -- the SAME value the dispatcher holds.
    #[must_use]
    pub fn cursor(&self) -> Arc<ClientSeqCursor> {
        self.services.client_seq_cursor(WORKER_FP)
    }

    /// Mark this generation ready, as a published snapshot does.
    pub fn mark_ready(&self) {
        self.handle.mark_ready();
    }

    /// Every downstream frame the socket was handed.
    #[must_use]
    pub fn sent(&self) -> Vec<CoordWorkerDownstream> {
        self.socket
            .frames()
            .into_iter()
            .map(|frame| frame.0)
            .collect()
    }

    /// The `client_seq` values acknowledged, in order.
    #[must_use]
    pub fn acks(&self) -> Vec<u64> {
        self.sent()
            .into_iter()
            .filter_map(|frame| match frame {
                CoordWorkerDownstream::EventAck(ack) => Some(ack.client_seq),
                _ => None,
            })
            .collect()
    }

    /// How many `events` rows this worker's sequence wrote.
    pub async fn rows_for(&self, client_seq: u64) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM events WHERE worker_fp = ? AND client_seq = ?",
        )
        .bind(WORKER_FP)
        .bind(client_seq as i64)
        .fetch_one(self.database.pool())
        .await
        .expect("the count query runs")
    }
}

impl Drop for LinkFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The worker row a durable write's tenancy triggers need.
fn enroll_sql(worker_fp: &str) -> String {
    format!(
        "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
         VALUES ('{worker_fp}', 'link-fixture', 'linux', 1000, 1000, \
         (SELECT id FROM dashboards LIMIT 1))"
    )
}

/// A fingerprint the brand accepts.
pub fn worker(value: &str) -> WorkerFp {
    WorkerFp::try_from(value.to_owned()).expect("a 64-hex fingerprint")
}

/// A session id the brand accepts.
pub fn session_id() -> SessionId {
    SessionId::try_from(SESSION_ID.to_owned()).expect("a UUID session id")
}

/// An open session row, as a snapshot announces one.
pub fn live_session(worker_fp: &WorkerFp, channel: i64) -> Session {
    Session {
        id: session_id(),
        worker_fp: worker_fp.clone(),
        channel: ChannelId::try_from(channel).expect("the fixture channel fits"),
        kind: SessionKind::Shell,
        cwd: "/tmp".to_owned(),
        spawn_cwd: None,
        workspace_id: None,
        status: SessionStatus::Open,
        created_at: 1_000,
        closed_at: None,
        custom_title: None,
        git_branch: None,
        git_remote: None,
        pr_number: None,
        pr_state: None,
        pr_checks: None,
        pr_url: None,
        ports: None,
    }
}

/// A durable frame carrying one event at one sequence.
pub fn event_frame(event: SessionEvent, client_seq: u64) -> InboundFrame {
    encoded(
        FrameClass::Durable,
        channel_of(&event),
        CoordWorkerUpstream::Event {
            event,
            client_seq,
            trace_id: None,
        },
    )
}

/// A live frame on `channel`.
pub fn live_frame(channel: u32, upstream: CoordWorkerUpstream) -> InboundFrame {
    encoded(FrameClass::Live, channel, upstream)
}

/// An rpc reply frame.
pub fn rpc_frame(upstream: CoordWorkerUpstream) -> InboundFrame {
    encoded(FrameClass::Rpc, 0, upstream)
}

fn encoded(class: FrameClass, channel: u32, upstream: CoordWorkerUpstream) -> InboundFrame {
    InboundFrame {
        class,
        channel,
        frame: upstream,
    }
}

/// The channel a durable event names, which is the header's channel too.
fn channel_of(event: &SessionEvent) -> u32 {
    match event {
        SessionEvent::Opened { channel, .. } => channel.as_u32(),
        SessionEvent::Respawned { new_channel, .. } => new_channel.as_u32(),
        _ => 0,
    }
}

/// Live effects that announce the moment the append reaches them and then hold
/// there, so a test can read the coordinator's state from inside the append.
///
/// The hold is a `Barrier` and not a sleep: the test needs the append parked at
/// a KNOWN point, and a sleep only makes that likely.
pub struct ParkedEffects {
    reached: Sender<()>,
    release: Arc<Barrier>,
    reaped: Mutex<Vec<String>>,
}

impl ParkedEffects {
    /// Effects that announce entry and park until `release` has two parties.
    #[must_use]
    pub fn new(reached: Sender<()>, release: Arc<Barrier>) -> Arc<Self> {
        Arc::new(Self {
            reached,
            release,
            reaped: Mutex::new(Vec::new()),
        })
    }

    /// The session ids a kill was dispatched for.
    #[must_use]
    pub fn reaped(&self) -> Vec<String> {
        self.reaped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl LiveEffects for ParkedEffects {
    fn index_durable_channel(
        &self,
        _event: &SessionEvent,
        _authenticated_worker_fp: Option<&WorkerFp>,
    ) {
        // Announce, then park. The test is now holding a thread inside the
        // append, which is the only moment at which "the slot was offered
        // before the append ran" is a question about two different instants.
        let _ = self.reached.send(());
        self.release.wait();
    }

    fn kill_orphan_pty(&self, _worker_fp: &WorkerFp, session_id: &str) {
        self.reaped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(session_id.to_owned());
    }
}
