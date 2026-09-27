//! What one event-core test runs against: a temporary coordinator database, the
//! synchronous second connection that decides whether a durable row is visible
//! yet, the live-effects recorder, and the observation log both of them write
//! into. The values those tests assert on -- ids and event shapes -- are built
//! by the `builders` sibling instead, because they are values rather than state.

#![allow(dead_code)]
// Every expect here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use roost_coord::db::{CoordDb, open as open_database};
use roost_coord::events::append::{AppendOptions, LiveEffects};
use roost_coord::events::bus_domains::Buses;
use roost_coord::events::pending_publications::PendingPublicationStore;
use roost_protocol::wire::{SessionEvent, WorkerFp};
use sqlx::Row;

// Through the parent's index rather than straight at the sibling module: the
// index is what the test binaries import, and one path to a name means the two
// halves cannot drift into a cycle.
use super::{DASHBOARD_ID, ORGANIZATION_ID, SyncReader, fingerprint};

/// Re-exported rather than re-imported: a caller names a writer, so the type
/// belongs to the same concern as the fixture it is stamped onto, and
/// `builders::worker_caller` is its only other user.
pub use roost_coord::events::append::Caller;

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// One test's coordinator: a writer pool of one, a synchronous reader over the
/// same file, the buses, the publication store, and the observation log.
pub struct EventFixture {
    /// The handle appends run on.
    pub writer: CoordDb,
    /// The synchronous second connection; see the recorder below for why it is a
    /// thread rather than an await.
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

// WHY THE VISIBILITY PROBE IS A THREAD AND NOT AN AWAIT.
// `LiveEffects::index_durable_channel` is synchronous, as it is in v2, so the
// probe cannot await. `SyncReader` therefore owns a raw `SqliteConnection` on a
// dedicated OS thread with its own current-thread runtime, and this recorder
// hands it a question over a channel and blocks for the answer.
// `Handle::block_on` inside a runtime worker would have been the shorter
// spelling and the wrong one: it is documented as panicking in an async
// context, and a probe that panics proves nothing. v2 reaches the same pair of
// handles the same way in `apps/coord/tests/durable-publication-fixture.ts`.

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