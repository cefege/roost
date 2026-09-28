// A coordinator serving its real router on an ephemeral port, with the
// services exposed so a Sync socket test can publish onto the buses the
// socket listens to, and a browser key enrolled so the upgrade verifies.
//
// Shared by the `sync_ws_socket*` binaries. The client half is
// `ws_client_support` (dial, read, close codes) and the credential half is
// `ws_credential_support` (JWT minting); this file owns only the server and
// the frame helpers a Sync test reads with.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use roost_coord::coord_core::CoordCore;
use roost_coord::coord_core::boot_facts::BootFacts;
use roost_coord::events::bus_messages::{TaskBusMsg, TaskBusMsgKind};
use roost_coord::http::listener::{ListenerState, build_router};
use roost_coord::http::spa::SpaMount;
use roost_coord::rpc::service::CoordinatorServiceImpl;
use roost_coord::services::CoordServices;
use roost_host::{CoordConfig, CoordConfigInput};
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::sync_client_frame::Command;
use roost_proto::buffa::Message as _;
use roost_proto::{
    FirehoseFrame, SyncClientFrame, SyncDomain, SyncDomainReadyCommand, SyncSubscribedFrame, Task,
};
use sqlx::AssertSqlSafe;
use tokio_tungstenite::tungstenite::Message;

mod canonical_bytes;

use canonical_bytes::canonical_client_bytes;

use crate::ws_client_support::{Dialed, WsClient, dial, next_frame, send_binary};
use crate::ws_credential_support::{mint_coordinator_jwt, now_secs};

/// How long a test waits for a frame it expects.
pub const EXPECT: Duration = Duration::from_secs(5);

/// How long a test watches for a frame it expects NOT to arrive.
pub const QUIET: Duration = Duration::from_millis(300);

/// The Sync path.
pub const SYNC_PATH: &str = "/ws/coord-sync";

/// A coordinator on a real port.
pub struct SyncFixture {
    pub services: Arc<CoordServices>,
    pub address: SocketAddr,
    account_id: String,
    root: PathBuf,
    server: tokio::task::JoinHandle<()>,
}

impl SyncFixture {
    /// Boot a coordinator and serve its router.
    pub async fn start(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-sync-socket-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database_path = root.join("coord.db");
        let database = roost_coord::db::open(&database_path)
            .await
            .expect("a migrated database");
        let resolved = CoordConfig::parse(CoordConfigInput {
            bind: Some("127.0.0.1:0".to_owned()),
            db_path: Some(database_path),
            authorized_keys_path: Some(root.join("authorized_keys")),
            log_dir: Some(root.join("logs")),
            ..CoordConfigInput::default()
        })
        .expect("a coordinator config");
        let tenant = roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 0)
            .await
            .expect("the self-hosted tenant");
        let account_id = tenant.account_id.clone();
        let services = Arc::new(CoordServices::booted(
            database,
            BootFacts {
                tenant: Some(tenant),
                config: Some(Arc::new(resolved.clone())),
                process_epoch: "epoch-1".to_owned(),
                boot_ms: 0,
            },
        ));
        let service = Arc::new(CoordinatorServiceImpl::new(
            CoordCore::new(Arc::clone(&services)),
            resolved.clone(),
            "epoch-1".to_owned(),
            0,
            "sha".to_owned(),
        ));
        let state = Arc::new(ListenerState {
            service,
            services: Arc::clone(&services),
            bind: resolved.bind.clone(),
            web_public_url: resolved.web_public_url.clone(),
            trust_proxy: resolved.trust_proxy,
            spa: Arc::new(SpaMount::from_dist_path(None)),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a bound listener");
        let address = listener.local_addr().expect("the bound address");
        let mounted = build_router(state);
        mounted.publish_bound_port(address.port());
        let server = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                mounted
                    .router
                    .into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await;
        });
        Self {
            services,
            address,
            account_id,
            root,
            server,
        }
    }

    /// Enroll a paired browser for the key derived from `seed`, and return
    /// its fingerprint and a fresh credential for it.
    pub async fn enroll_browser(&self, seed: u8) -> (String, String) {
        let now = now_secs();
        let (fingerprint, public_key, token) = mint_coordinator_jwt([seed; 32], now, now + 300);
        self.exec(&format!(
            "INSERT INTO authorized_keys (fingerprint, public_key, label, added_at, \
             paired_from_ip, paired_country) VALUES ('{fingerprint}', x'{}', 'browser', 1000, \
             '203.0.113.9', 'SE')",
            hex::encode(public_key),
        ))
        .await;
        self.exec(&format!(
            "INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms) \
             VALUES ('{fingerprint}', '{}', 1000, 1000)",
            self.account_id
        ))
        .await;
        (fingerprint, token)
    }

    async fn exec(&self, sql: &str) {
        sqlx::query(AssertSqlSafe(sql))
            .execute(self.services.db.pool())
            .await
            .expect("the statement applies");
    }

    /// Dial the Sync path with `query` (no leading `?`), offering the marker
    /// and `token` exactly as a browser does.
    pub async fn dial_sync(&self, query: &str, token: &str) -> Dialed {
        let path = if query.is_empty() {
            SYNC_PATH.to_owned()
        } else {
            format!("{SYNC_PATH}?{query}")
        };
        dial(self.address, &path, &["roost-auth", token]).await
    }

    /// Publish one task row whose payload is `payload_bytes` long.
    pub fn publish_task(&self, id: &str, payload_bytes: usize) {
        self.services.buses.task_bus.publish(TaskBusMsg {
            kind: TaskBusMsgKind::Created,
            task: Task {
                id: id.to_owned(),
                state: "pending".to_owned(),
                payload_json: "x".repeat(payload_bytes),
                ..Task::default()
            },
        });
    }
}

impl Drop for SyncFixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The next frame as a decoded `FirehoseFrame`, or `None` inside `bound`.
pub async fn next_firehose(socket: &mut WsClient, bound: Duration) -> Option<FirehoseFrame> {
    match next_frame(socket, bound).await? {
        Message::Binary(bytes) => {
            Some(FirehoseFrame::decode_from_slice(&bytes).expect("a firehose frame"))
        }
        other => panic!("expected a binary Sync frame, got {other:?}"),
    }
}

/// Read the `subscribed` barrier a v2 socket opens with.
pub async fn read_subscribed(socket: &mut WsClient) -> SyncSubscribedFrame {
    let frame = next_firehose(socket, EXPECT)
        .await
        .expect("the subscribed barrier");
    assert_eq!(frame.delivery_seq, 0, "the barrier is a control");
    match frame.frame {
        Some(Frame::Subscribed(subscribed)) => *subscribed,
        other => panic!("expected subscribed first, got {other:?}"),
    }
}

/// One domain's generation from the barrier.
pub fn generation_of(subscribed: &SyncSubscribedFrame, domain: SyncDomain) -> u64 {
    subscribed
        .generations
        .iter()
        .find(|entry| entry.domain.as_known() == Some(domain))
        .map(|entry| entry.generation)
        .expect("the domain is announced")
}

/// Send one client frame on `socket_id`.
pub async fn send_client_frame(
    socket: &mut WsClient,
    socket_id: &str,
    ack: Option<u64>,
    command: Option<Command>,
) {
    let frame = SyncClientFrame {
        socket_id: socket_id.to_owned(),
        ack_delivery_seq: ack,
        command,
        ..SyncClientFrame::default()
    };
    send_binary(socket, canonical_client_bytes(&frame)).await;
}

/// `domain_ready` for a domain with no snapshot fence.
pub fn domain_ready(domain: SyncDomain, generation: u64) -> Command {
    Command::DomainReady(Box::new(SyncDomainReadyCommand {
        domain: domain.into(),
        generation,
        ..SyncDomainReadyCommand::default()
    }))
}

/// The task id a `task_delta` frame carries.
pub fn task_id_of(frame: &FirehoseFrame) -> String {
    use roost_proto::__buffa::oneof::task_delta_proto::Kind;
    match &frame.frame {
        Some(Frame::TaskDelta(delta)) => match &delta.kind {
            Some(Kind::Created(task) | Kind::State(task)) => task.id.clone(),
            None => panic!("a task delta with no kind"),
        },
        other => panic!("expected a task delta, got {other:?}"),
    }
}
