//! Isolated state for the event-core tests: a temporary coordinator database
//! with a **second connection the tests can read synchronously**, the live-effects
//! recorder, and the fixtures the append path needs.
//!
//! The second connection is the point of the whole module. A WAL reader sees only
//! committed transactions, so asking it "is this event row visible?" *during* a
//! publication answers the ordering question directly rather than by inspecting
//! the code. `apps/coord/tests/durable-publication-fixture.ts` uses the same pair
//! of handles for the same reason.
//!
//! WHY THAT READER IS A THREAD AND NOT AN AWAIT. `LiveEffects::index_durable_channel`
//! is synchronous, as it is in v2, so the probe cannot await. The reader therefore
//! owns a raw `SqliteConnection` on a dedicated OS thread with its own
//! current-thread runtime, and the recorder hands it a question over a channel and
//! blocks for the answer. `Handle::block_on` inside a runtime worker would have
//! been the shorter spelling and the wrong one: it is documented as panicking in
//! an async context, and a probe that panics proves nothing.
//!
//! Everything here is a test fixture, so the ids are the shapes the protocol's
//! brands require (a fingerprint is 64 hex characters, a session id is a UUID) and
//! the tenancy rows exist because the schema's triggers refuse a write that has no
//! dashboard scope -- the triggers are the point, and a fixture that bypassed them
//! would be testing a schema nobody ships.

#![allow(dead_code)]
// Every expect here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

mod reader;

pub use reader::SyncReader;

use roost_coord::db::{CoordDb, open as open_database};
use roost_coord::events::append::{AppendOptions, Caller, LiveEffects};
use roost_coord::events::bus_domains::Buses;
use roost_coord::events::pending_publications::PendingPublicationStore;
use roost_protocol::wire::{SessionEvent, SessionId, WorkerFp, WorkspaceId};
use sqlx::Row;

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The dashboard every fixture row is scoped to.
pub const DASHBOARD_ID: &str = "00000000-0000-4000-8000-0000000000d1";
/// The organization that owns it.
pub const ORGANIZATION_ID: &str = "00000000-0000-4000-8000-0000000000c1";

/// A worker fingerprint: 64 lowercase hex characters.
pub fn fingerprint(byte: char) -> WorkerFp {
    WorkerFp::try_from(byte.to_string().repeat(64)).expect("a repeated hex digit is a fingerprint")
}

/// A session id: a UUID, because the brand checks the shape.
pub fn session_id(last: char) -> SessionId {
    SessionId::try_from(format!("00000000-0000-4000-8000-00000000000{last}"))
        .expect("the fixture id is a UUID")
}

/// A workspace id: a UUID, for the same reason.
pub fn workspace_id(last: char) -> WorkspaceId {
    WorkspaceId::try_from(format!("00000000-0000-4000-8000-00000000001{last}"))
        .expect("the fixture id is a UUID")
}

/// One test's coordinator: a writer pool of one, a synchronous reader over the
/// same file, the buses, the publication store, and the observation log.
pub struct EventFixture {
    /// The handle appends run on.
    pub writer: CoordDb,
    /// The synchronous second connection. See the module header.
    pub reader: SyncReader,
    /// The buses the publication half publishes to.
    pub buses: Arc<Buses>,
    /// The bounded publication store.
    pub publications: Arc<Mutex<PendingPublicationStore>>,
    /// What the live effects did, in order.
    pub observed: Arc<Mutex<Observation>>,
    directory: PathBuf,
}

/// What the live effects and the bus saw, in the order they saw it.
#[derive(Debug, Default)]
pub struct Observation {
    /// One entry per live effect, in call order.
    pub steps: Vec<Step>,
}

/// One recorded step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// The durable channel index was applied. The flag is what the *second
    /// connection* could see of the event at that instant: true means the
    /// transaction had committed.
    Indexed {
        /// The event kind the index was handed.
        kind: String,
        /// The fingerprint the index was told, never one the event claimed.
        authenticated_worker_fp: Option<String>,
        /// Whether a separate connection could already see the durable row.
        committed: bool,
    },
    /// The session bus was published to.
    SessionPublished {
        /// The event kind.
        kind: String,
        /// The durable id the message was stamped with.
        event_id: Option<u64>,
    },
    /// The workspace bus was published to.
    WorkspacePublished {
        /// The workspace the delta names.
        id: String,
    },
    /// An orphan PTY kill was dispatched.
    Reaped {
        /// The session that was killed.
        session_id: String,
    },
}

impl EventFixture {
    /// A migrated database in a directory of its own, with the tenancy rows a
    /// worker-scoped write needs.
    pub async fn new(slug: &str) -> Self {
        let unique = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "roost-events-{slug}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).expect("the fixture directory is creatable");
        let path = directory.join("coord.db");
        let writer = open_database(&path)
            .await
            .expect("the fixture database opens");
        seed_tenancy(&writer).await;
        let reader = SyncReader::open(&path);
        Self {
            writer,
            reader,
            buses: Buses::shared(),
            publications: Arc::new(Mutex::new(PendingPublicationStore::new())),
            observed: Arc::new(Mutex::new(Observation::default())),
            directory,
        }
    }

    /// The append options a test uses by default: this fixture's clock, buses,
    /// publication store, and live effects.
    pub fn options<'a>(&'a self, live_effects: &'a dyn LiveEffects) -> AppendOptions<'a> {
        AppendOptions {
            now_ms: 1_700_000_000_000,
            buses: &self.buses,
            live_effects,
            pending_publications: Some(Arc::clone(&self.publications)),
            can_publish: None,
            extra_work: None,
            defer_snapshot_reap: false,
        }
    }

    /// How many durable rows the `events` table holds for one worker's sequence.
    pub async fn rows_for(&self, worker_fp: &WorkerFp, client_seq: u64) -> i64 {
        let row = sqlx::query("SELECT COUNT(*) FROM events WHERE worker_fp = ? AND client_seq = ?")
            .bind(worker_fp.as_str())
            .bind(client_seq as i64)
            .fetch_one(self.writer.pool())
            .await
            .expect("the count query runs");
        row.get::<i64, _>(0)
    }

    /// Every durable event id, oldest first.
    pub async fn event_ids(&self) -> Vec<i64> {
        let rows = sqlx::query("SELECT id FROM events ORDER BY id ASC")
            .fetch_all(self.writer.pool())
            .await
            .expect("the id query runs");
        rows.iter().map(|row| row.get::<i64, _>(0)).collect()
    }

    /// Record a step from outside the live effects -- a bus subscription, which is
    /// the only way to see that the session bus was published to and in what order
    /// relative to the durable channel index.
    pub fn record(&self, step: Step) {
        self.observed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .steps
            .push(step);
    }

    /// The recorded steps.
    pub fn steps(&self) -> Vec<Step> {
        self.observed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .steps
            .clone()
    }

    /// Stop the reader and remove the fixture's directory.
    ///
    /// It borrows, because the recorded effects and the bus subscriptions a test
    /// is holding have already borrowed the fixture: a `close(self)` here would
    /// make every test that keeps either one fail to compile for a reason that has
    /// nothing to do with the test.
    pub fn close(&self) {
        self.reader.stop();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// The tenancy rows a worker-scoped write needs: the organization, the dashboard,
/// and two live workers. The schema's triggers refuse a session or an event with
/// no dashboard scope, and refuse a session whose worker is scoped elsewhere, so
/// a fixture without these rows cannot exercise anything.
pub async fn seed_tenancy(database: &CoordDb) {
    sqlx::query(
        "INSERT INTO organizations (id, slug, name, status, created_at_ms) \
         VALUES (?, ?, ?, 'active', 1)",
    )
    .bind(ORGANIZATION_ID)
    .bind("fixture-org")
    .bind("Fixture Organization")
    .execute(database.pool())
    .await
    .expect("the organization inserts");
    sqlx::query(
        "INSERT INTO dashboards (id, organization_id, slug, name, status, created_at_ms) \
         VALUES (?, ?, ?, ?, 'active', 1)",
    )
    .bind(DASHBOARD_ID)
    .bind(ORGANIZATION_ID)
    .bind("fixture")
    .bind("Fixture")
    .execute(database.pool())
    .await
    .expect("the dashboard inserts");
    for byte in ['d', 'e'] {
        sqlx::query(
            "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
             VALUES (?, 'fixture', 'linux', 1, 1, ?)",
        )
        .bind(fingerprint(byte).as_str())
        .bind(DASHBOARD_ID)
        .execute(database.pool())
        .await
        .expect("the worker inserts");
    }
}

/// The live effects a test installs: it records what it was asked to do, and for
/// the channel index it asks the second connection whether the durable row is
/// visible yet.
pub struct RecordingEffects {
    reader: Arc<SyncReader>,
    observed: Arc<Mutex<Observation>>,
}

impl RecordingEffects {
    /// Record into the fixture's observation log, reading visibility through the
    /// fixture's second connection.
    pub fn new(fixture: &EventFixture) -> Self {
        Self {
            reader: Arc::new(SyncReader::open(fixture.writer.path())),
            observed: Arc::clone(&fixture.observed),
        }
    }

    fn record(&self, step: Step) {
        self.observed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .steps
            .push(step);
    }
}

impl LiveEffects for RecordingEffects {
    fn index_durable_channel(
        &self,
        event: &SessionEvent,
        authenticated_worker_fp: Option<&WorkerFp>,
    ) {
        let worker_fp = authenticated_worker_fp.map(WorkerFp::to_string);
        // A coordinator-side producer carries no fingerprint, so there is no
        // sequence to look up; its row is checked by the tests that care.
        let committed = match &worker_fp {
            Some(worker_fp) => self.reader.row_is_committed(
                worker_fp,
                &serde_json::to_string(event).expect("an event encodes"),
            ),
            None => true,
        };
        self.record(Step::Indexed {
            kind: event.kind_name().to_owned(),
            authenticated_worker_fp: worker_fp,
            committed,
        });
    }

    fn kill_orphan_pty(&self, _worker_fp: &WorkerFp, session_id: &str) {
        self.record(Step::Reaped {
            session_id: session_id.to_owned(),
        });
    }
}

/// An `opened` event on one channel.
pub fn opened_event(session: &SessionId, worker_fp: &WorkerFp, channel: i64) -> SessionEvent {
    SessionEvent::Opened {
        session_id: session.clone(),
        worker_fp: worker_fp.clone(),
        channel: roost_protocol::wire::ChannelId::try_from(channel)
            .expect("the fixture channel fits"),
        session_kind: roost_protocol::wire::SessionKind::Shell,
        cwd: "/tmp".to_owned(),
        ts: 1,
        trace_id: None,
    }
}

/// A `respawned` event onto a new keeper channel.
pub fn respawned_event(session: &SessionId, channel: i64) -> SessionEvent {
    SessionEvent::Respawned {
        session_id: session.clone(),
        new_channel: roost_protocol::wire::ChannelId::try_from(channel)
            .expect("the fixture channel fits"),
        ts: 2,
        trace_id: None,
    }
}

/// A `closed` event: the terminal exited.
pub fn closed_event(session: &SessionId) -> SessionEvent {
    SessionEvent::Closed {
        session_id: session.clone(),
        exit_code: Some(0),
        ts: 3,
        trace_id: None,
    }
}

/// The live row a snapshot announces for one session.
pub fn live_session(
    session: &SessionId,
    worker_fp: &WorkerFp,
    channel: i64,
    workspace: Option<&WorkspaceId>,
) -> roost_protocol::wire::Session {
    roost_protocol::wire::Session {
        id: session.clone(),
        worker_fp: worker_fp.clone(),
        channel: roost_protocol::wire::ChannelId::try_from(channel)
            .expect("the fixture channel fits"),
        kind: roost_protocol::wire::SessionKind::Shell,
        cwd: "/tmp".to_owned(),
        spawn_cwd: None,
        workspace_id: workspace.cloned(),
        status: roost_protocol::wire::SessionStatus::Open,
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

/// A `snapshot` event announcing a worker's whole live set.
pub fn snapshot_event(
    worker_fp: &WorkerFp,
    sessions: Vec<roost_protocol::wire::Session>,
) -> SessionEvent {
    SessionEvent::Snapshot {
        worker_fp: worker_fp.clone(),
        sessions,
        ts: 5_000,
        trace_id: None,
    }
}

/// A worker caller on one outbox sequence.
pub fn worker_caller(worker_fp: &WorkerFp, client_seq: u64) -> Caller {
    Caller::worker(worker_fp.clone(), client_seq, DASHBOARD_ID)
}
