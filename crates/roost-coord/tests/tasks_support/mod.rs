// The fixture every task-queue test drives: a migrated database with the
// self-hosted tenant booted, a `CoordCore` over it, and a subscription on the
// task bus that records what a Sync socket would have received.
//
// The subscription is the point of this file rather than a convenience. The Sync
// firehose (`sync_ws::feed`) reaches task state through `task_bus` and nothing
// else, so a publication this fixture observes is a publication every connected
// queue view receives -- which is exactly what
// `docs/FAILURE-INDEX.md` ("task state changes invisible to other browsers")
// was about.
//
// `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
// test is its own crate rather than a module of one, so the exemption has to be
// stated here rather than inherited.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use roost_coord::auth::principal::Principal;
use roost_coord::coord_core::boot_facts::BootFacts;
use roost_coord::coord_core::{Caller, CoordCore, ListenerTrust};
use roost_coord::db::CoordDb;
use roost_coord::events::bus_messages::TaskBusMsg;
use roost_coord::services::CoordServices;
use roost_coord::sessions::tasks::{
    handle_tasks_enqueue, handle_tasks_list, handle_tasks_next_pending,
};
use sqlx::AssertSqlSafe;

pub const DEVICE_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const OTHER_DEVICE_FP: &str =
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const WORKER_FP: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

/// A coordinator over a scratch database, with the tenant booted.
pub struct TasksFixture {
    /// The only way into a handler.
    pub core: CoordCore,
    /// The database handle, for the rows a test asserts on directly.
    pub database: CoordDb,
    received: Arc<Mutex<Vec<TaskBusMsg>>>,
    root: std::path::PathBuf,
}

impl TasksFixture {
    /// A fixture with the self-hosted tenant and an empty queue.
    pub async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-tasks-{label}-{}-{:?}",
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
        let services = Arc::new(CoordServices::booted(
            database.clone(),
            BootFacts {
                tenant: Some(tenant),
                process_epoch: "epoch-under-test".to_owned(),
                boot_ms: 1_000,
                ..Default::default()
            },
        ));
        let core = CoordCore::new(Arc::clone(&services));
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&received);
        services.buses.task_bus.subscribe(move |message: &TaskBusMsg| {
            sink.lock().expect("the recording lock").push(message.clone());
        });
        Self {
            core,
            database,
            received,
            root,
        }
    }

    /// Insert a task row straight into the table, as a previous process would
    /// have left it: the RPCs are the only thing under test, not the enqueue.
    pub async fn seed(&self, id: &str, state: &str, enqueued_at_ms: i64, claimed_by: Option<&str>) {
        sqlx::query(AssertSqlSafe(format!(
            "INSERT INTO tasks (id, dashboard_id, state, payload_json, enqueued_at_ms, \
             claimed_at_ms, claimed_by, claim_ttl_ms) \
             VALUES ('{id}', (SELECT id FROM dashboards LIMIT 1), '{state}', '{{}}', \
             {enqueued_at_ms}, NULL, {}, 900000)"
        )))
        .bind(claimed_by)
        .execute(self.database.pool())
        .await
        .expect("the row is seeded");
    }

    /// Enqueue through the real handler, and answer with the new task's id.
    pub async fn enqueue(&self, payload: &str) -> String {
        handle_tasks_enqueue(
            &self.core,
            &device(DEVICE_FP),
            roost_proto::TasksEnqueueRequest {
                payload_json: payload.to_owned(),
                ..Default::default()
            },
        )
        .await
        .expect("an enqueued task")
        .body
        .task
        .into_option()
        .expect("a task")
        .id
    }

    /// Forget what has been recorded, so a test can assert about one call.
    pub fn forget_deltas(&self) {
        self.received.lock().expect("the recording lock").clear();
    }

    /// Every bus message a Sync subscriber would have received, in order.
    pub fn deltas(&self) -> Vec<TaskBusMsg> {
        self.received.lock().expect("the recording lock").clone()
    }
}

impl Drop for TasksFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The browser that acts for an account.
pub fn device(fingerprint: &str) -> Caller {
    caller_with(Principal::AccountDevice {
        fingerprint: fingerprint.to_owned(),
        label: "test device".to_owned(),
        account_id: "acct_self_hosted".to_owned(),
    })
}

/// The machine credential, which is not a browser.
pub fn machine(fingerprint: &str) -> Caller {
    caller_with(Principal::Worker {
        fingerprint: fingerprint.to_owned(),
        label: "test worker".to_owned(),
    })
}

fn caller_with(principal: Principal) -> Caller {
    Caller {
        principal,
        tab_id: None,
        remote_address: None,
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}

/// The oldest queued task, claimed by `DEVICE_FP`.
pub async fn claim_next(fixture: &TasksFixture) -> roost_proto::Task {
    handle_tasks_next_pending(
        &fixture.core,
        &device(DEVICE_FP),
        roost_proto::TasksNextPendingRequest::default(),
    )
    .await
    .expect("a claim")
    .body
    .task
    .into_option()
    .expect("a claimed task")
}

/// The `state` column of one stored row, read past the RPC layer.
pub async fn stored_state(fixture: &TasksFixture, id: &str) -> String {
    sqlx::query_scalar(AssertSqlSafe(format!("SELECT state FROM tasks WHERE id = '{id}'")))
        .fetch_one(fixture.database.pool())
        .await
        .expect("the row")
}

/// The queue as `TasksList` answers it, optionally narrowed to one state.
pub async fn list(fixture: &TasksFixture, state: Option<&str>) -> Vec<roost_proto::Task> {
    handle_tasks_list(
        &fixture.core,
        &device(DEVICE_FP),
        roost_proto::TasksListRequest {
            state: state.map(str::to_owned),
            ..Default::default()
        },
    )
    .await
    .expect("a listed queue")
    .body
    .tasks
}
