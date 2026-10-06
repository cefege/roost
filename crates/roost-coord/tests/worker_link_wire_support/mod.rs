// A booted coordinator serving its REAL router on an ephemeral loopback port,
// with one enrolled worker whose credential a test dials the worker link with.
// Shared by the worker-link wire and heartbeat test binaries; depends on
// `ws_client_support` for the socket and `ws_credential_support` for the JWT.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use roost_coord::coord_core::CoordCore;
use roost_coord::coord_core::boot_facts::BootFacts;
use roost_coord::http::listener::{ListenerState, build_router};
use roost_coord::http::spa::SpaMount;
use roost_coord::rpc::service::CoordinatorServiceImpl;
use roost_coord::services::CoordServices;
use roost_coord::worker_link::upgrade_admission::WORKER_AUTH_SUBPROTOCOL;
use roost_host::{CoordConfig, CoordConfigInput};
use roost_protocol::proto_adapters::coord_worker_proto::{decode_downstream, encode_upstream};
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};
use tokio_tungstenite::tungstenite::Message;

use super::ws_client_support::{Dialed, WsClient, dial, next_frame, send_binary};
use super::ws_credential_support::{mint_coordinator_jwt, now_secs};

/// How long a test waits for a frame the coordinator is expected to send.
pub const FRAME_BOUND: Duration = Duration::from_secs(5);

/// One coordinator, one enrolled worker, and the listener serving both.
pub struct WireFixture {
    pub address: SocketAddr,
    pub services: Arc<CoordServices>,
    pub worker_fp: String,
    token: String,
    root: PathBuf,
    server: tokio::task::JoinHandle<()>,
}

impl WireFixture {
    /// Boot, enroll one worker with a fresh credential, and serve on `127.0.0.1:0`.
    pub async fn start(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-worker-wire-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database_location = super::db_support::test_database_location(&root).await;
        let database = roost_coord::db::open(&database_location)
            .await
            .expect("a migrated database");
        let config = CoordConfig::parse(CoordConfigInput {
            bind: Some("127.0.0.1:0".to_owned()),
            database: Some(database_location),
            authorized_keys_path: Some(root.join("authorized_keys")),
            log_dir: Some(root.join("logs")),
            ..CoordConfigInput::default()
        })
        .expect("a coordinator config");
        let tenant = roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 0)
            .await
            .expect("the self-hosted tenant");
        let services = Arc::new(CoordServices::booted(
            database,
            BootFacts {
                tenant: Some(tenant),
                config: Some(Arc::new(config.clone())),
                process_epoch: "epoch-1".to_owned(),
                boot_ms: 0,
            },
        ));
        let (worker_fp, public_key, token) =
            mint_coordinator_jwt([7; 32], now_secs() - 5, now_secs() + 3_600);
        enroll_worker(&services, &worker_fp, &public_key).await;

        let service = Arc::new(CoordinatorServiceImpl::new(
            CoordCore::new(Arc::clone(&services)),
            config.clone(),
            "epoch-1".to_owned(),
            0,
            "sha".to_owned(),
        ));
        let state = Arc::new(ListenerState {
            service,
            services: Arc::clone(&services),
            bind: config.bind.clone(),
            web_public_url: None,
            trust_proxy: false,
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
            address,
            services,
            worker_fp,
            token,
            root,
            server,
        }
    }

    /// The worker link's path for the enrolled worker.
    pub fn worker_path(&self) -> String {
        format!("/ws/coord-worker/{}", self.worker_fp)
    }

    /// Dial the worker link exactly as a worker does: marker, then credential.
    pub async fn dial_worker(&self) -> Dialed {
        dial(
            self.address,
            &self.worker_path(),
            &[WORKER_AUTH_SUBPROTOCOL, &self.token],
        )
        .await
    }

    /// An upgraded link that has sent its hello and read its hello-ack.
    pub async fn hello_link(&self) -> (WsClient, CoordWorkerDownstream) {
        let mut socket = self.dial_worker().await.socket();
        send_binary(&mut socket, hello_bytes(&self.worker_fp)).await;
        let ack = next_downstream(&mut socket).await.expect("a hello-ack");
        (socket, ack)
    }

    /// The branded fingerprint of the enrolled worker.
    pub fn fp(&self) -> WorkerFp {
        WorkerFp::try_from(self.worker_fp.clone()).expect("a 64-hex fingerprint")
    }
}

impl Drop for WireFixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The key row and the worker row a redeemed enrollment would have written.
async fn enroll_worker(services: &CoordServices, fingerprint: &str, public_key: &[u8; 32]) {
    super::db_support::insert_authorized_key(
        &services.db,
        fingerprint,
        public_key,
        "wire-worker",
        None,
    )
    .await;
    sqlx::query(
        "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
         VALUES ($1, 'wire-worker', 'linux', 1000, 1000, (SELECT id FROM dashboards LIMIT 1))",
    )
    .bind(fingerprint)
    .execute(services.db.pool())
    .await
    .expect("the enrollment row applies");
}

/// A `WHello` for `worker_fp`, advertising semantic metadata.
pub fn hello_bytes(worker_fp: &str) -> Vec<u8> {
    upstream_bytes(&CoordWorkerUpstream::Hello {
        worker_fp: WorkerFp::try_from(worker_fp.to_owned()).expect("a 64-hex fingerprint"),
        version: "wire-test".to_owned(),
        capabilities: vec![
            "terminal_metadata_v1".to_owned(),
            "a-capability-no-owner-serves".to_owned(),
        ],
        process_epoch: "worker-epoch-1".to_owned(),
        trace_id: None,
    })
}

/// A `WPong` carrying `ts`.
pub fn pong_bytes(ts: i64) -> Vec<u8> {
    upstream_bytes(&CoordWorkerUpstream::Pong { ts, trace_id: None })
}

/// Any upstream frame as the bytes a worker writes.
pub fn upstream_bytes(frame: &CoordWorkerUpstream) -> Vec<u8> {
    encode_upstream(frame).expect("the test frame encodes")
}

/// The next coordinator frame, decoded; `None` for a close, an end, or silence.
pub async fn next_downstream(socket: &mut WsClient) -> Option<CoordWorkerDownstream> {
    match next_frame(socket, FRAME_BOUND).await? {
        Message::Binary(bytes) => Some(decode_downstream(&bytes).expect("a coordinator frame")),
        _ => None,
    }
}
