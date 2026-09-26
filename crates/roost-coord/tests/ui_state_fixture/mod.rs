//! The UI tests' shared fixture: a migrated coordinator database with a
//! self-hosted tenant, one worker, one persisted session, and a UI state
//! runtime whose clock the test drives.
//!
//! Owned by the UI tests. The clock matters as much as the database here: both
//! TTLs under test -- a tab report's five minutes and a reserved apply's fifteen
//! seconds -- are wall-clock facts, and a test that waits for one is a test that
//! proves nothing.

// The fixture is compiled into every `ui_state_*.rs` test binary and each uses a
// different subset of it, so an item unused by ONE of them is not dead.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code, unused_imports)]

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use roost_coord::auth::principal::Principal;
use roost_coord::coord_core::{Caller, CoordCore, ListenerTrust};
use roost_coord::db::CoordDb;
use roost_coord::events::bus_messages::UiBusMsg;
use roost_coord::services::CoordServices;
use roost_coord::ui_state::UiStateRuntime;
use roost_proto as proto;
use roost_proto::buffa::MessageField;
use roost_proto::__buffa::oneof::layout_document_node::Node;
use roost_proto::__buffa::oneof::ui_command::Command;

/// The session every persisted-session check resolves against.
pub const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";

/// A one-leaf layout document binding `session_id` to one slot.
pub fn layout_document(session_id: &str) -> proto::LayoutDocumentV1 {
    proto::LayoutDocumentV1 {
        schema_version: 1,
        root: MessageField::some(proto::LayoutDocumentNode {
            node: Some(Node::Leaf(Box::new(proto::LayoutDocumentLeaf {
                leaf_key: "leaf-1".to_owned(),
                slot_keys: vec!["slot-1".to_owned()],
                selected_slot_key: Some("slot-1".to_owned()),
                ..Default::default()
            }))),
            ..Default::default()
        }),
        focused_leaf_key: "leaf-1".to_owned(),
        bindings: vec![proto::LayoutDocumentBinding {
            slot_key: "slot-1".to_owned(),
            session_id: session_id.to_owned(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// A report naming `tab_id`, with the given document attached or absent.
pub fn report_request(
    tab_id: &str,
    active_path: &str,
    document: Option<proto::LayoutDocumentV1>,
) -> proto::UiReportStateRequest {
    proto::UiReportStateRequest {
        tab_id: tab_id.to_owned(),
        active_path: active_path.to_owned(),
        folder_key: "fingerprint:/tmp/project".to_owned(),
        layout_document: document.map(MessageField::some).unwrap_or_else(MessageField::none),
        ..Default::default()
    }
}

/// A legacy `selectTab` command naming one session.
pub fn select_tab_command(session_id: &str) -> proto::UiCommand {
    proto::UiCommand {
        command: Some(Command::SelectTab(Box::new(proto::UiSelectTab {
            session_id: session_id.to_owned(),
            ..Default::default()
        }))),
        ..Default::default()
    }
}

/// A session id that is well formed and persisted nowhere.
pub const FOREIGN_SESSION_ID: &str = "99999999-9999-4999-8999-999999999999";

/// A browser device fingerprint, as the key table's hex form.
pub fn browser_fingerprint(seed: char) -> String {
    std::iter::repeat_n(seed, 64).collect()
}

/// A caller that passed the device gate, with or without a tab fence.
pub fn browser_caller(fingerprint: &str, account_id: &str, tab_id: Option<&str>) -> Caller {
    Caller {
        principal: Principal::AccountDevice {
            fingerprint: fingerprint.to_owned(),
            label: "ui-state-test".to_owned(),
            account_id: account_id.to_owned(),
        },
        tab_id: tab_id.map(str::to_owned),
        remote_address: Some("127.0.0.1".to_owned()),
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}

/// A coordinator with UI state, a persisted session, and a test-driven clock.
pub struct UiStateFixture {
    /// The coordinator's shared state, as a handler receives it.
    pub core: CoordCore,
    /// The retained reports and the reserved applies, over the test's clock.
    pub runtime: UiStateRuntime,
    /// The account the browser device belongs to.
    pub account_id: String,
    /// The scratch directory, removed when the fixture drops.
    root: PathBuf,
    /// The instant both owners read as "now".
    now_ms: Arc<Mutex<i64>>,
}

impl UiStateFixture {
    /// A fixture whose clock starts at zero.
    pub async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-ui-state-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated coordinator database");
        let tenant = roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 0)
            .await
            .expect("a self-hosted tenant");
        seed_worker(&database, &tenant.dashboard_id).await;
        seed_session(&database, &tenant.dashboard_id).await;
        seed_key(&database, &browser_fingerprint('a'), "Chrome - test").await;
        seed_key(&database, &browser_fingerprint('b'), "Firefox - test").await;

        let now_ms = Arc::new(Mutex::new(0_i64));
        let clock = {
            let now_ms = Arc::clone(&now_ms);
            Arc::new(move || *now_ms.lock().unwrap_or_else(|error| error.into_inner()))
        };
        let services = Arc::new(CoordServices::new(database));
        Self {
            core: CoordCore::new(services),
            runtime: UiStateRuntime::with_clock(clock),
            account_id: tenant.account_id,
            root,
            now_ms,
        }
    }

    /// The coordinator's database handle.
    pub fn database(&self) -> &CoordDb {
        &self.core.services.db
    }

    /// A tab-bound browser caller.
    pub fn caller(&self, seed: char) -> Caller {
        browser_caller(
            &browser_fingerprint(seed),
            &self.account_id,
            Some("tab-under-test"),
        )
    }

    /// A browser caller whose request carried no tab id.
    pub fn caller_without_tab(&self, seed: char) -> Caller {
        browser_caller(&browser_fingerprint(seed), &self.account_id, None)
    }

    /// Move both owners' clock forward.
    pub fn advance(&self, millis: i64) {
        let mut now = self.now_ms.lock().unwrap_or_else(|error| error.into_inner());
        *now += millis;
    }

    /// The instant the owners read as "now".
    pub fn now_ms(&self) -> i64 {
        *self.now_ms.lock().unwrap_or_else(|error| error.into_inner())
    }
}

impl Drop for UiStateFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn seed_worker(database: &CoordDb, dashboard_id: &str) {
    sqlx::query(
        "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
         VALUES ('ui-state-worker', 'UI state worker', 'linux', 0, 0, ?1)",
    )
    .bind(dashboard_id)
    .execute(database.pool())
    .await
    .expect("the worker row");
}

async fn seed_session(database: &CoordDb, dashboard_id: &str) {
    sqlx::query(
        "INSERT INTO sessions (id, worker_fp, channel, kind, cwd, status, created_at, dashboard_id) \
         VALUES (?1, 'ui-state-worker', 9, 'shell', '/ui', 'open', 0, ?2)",
    )
    .bind(SESSION_ID)
    .bind(dashboard_id)
    .execute(database.pool())
    .await
    .expect("the session row");
}

async fn seed_key(database: &CoordDb, fingerprint: &str, label: &str) {
    sqlx::query(
        "INSERT INTO authorized_keys (fingerprint, public_key, label, added_at) \
         VALUES (?1, ?2, ?3, 0)",
    )
    .bind(fingerprint)
    .bind(vec![1_u8; 32])
    .bind(label)
    .execute(database.pool())
    .await
    .expect("the authorized key row");
}

/// Run `body`, collecting every UI bus message published while it runs.
///
/// The subscription is taken before the body starts and dropped after it ends,
/// so the count a dispatch reports and the messages it published are the same
/// observation rather than two.
pub async fn collect_ui_bus<T>(
    fixture: &UiStateFixture,
    body: impl Future<Output = T>,
) -> (T, Vec<UiBusMsg>) {
    let seen: Arc<Mutex<Vec<UiBusMsg>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let subscription = fixture
        .core
        .services
        .buses
        .ui_bus
        .subscribe(move |message: &UiBusMsg| {
            sink.lock().expect("the bus sink lock").push(message.clone());
        });
    let outcome = body.await;
    drop(subscription);
    let messages = seen.lock().expect("the bus sink lock").clone();
    (outcome, messages)
}
