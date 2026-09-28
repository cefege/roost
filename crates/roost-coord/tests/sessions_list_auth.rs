//! `SessionsList` authority through the real coordinator router: a signed worker
//! JWT resolves to its worker principal and reads only its own open rows with
//! their private recovery metadata, and a browser device reads the rows but
//! never that metadata. Also the Sync snapshot binding a browser list asks for.
//!
//! Ports `apps/coord/tests/workers/worker-session-list-auth.test.ts` (the Guard
//! of FAILURE-INDEX "A worker reconnects but respawns every terminal") and the
//! `SessionsList` case of `apps/coord/tests/coord-bidi.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_ws_socket_support;
mod ws_client_support;
mod ws_credential_support;

use std::net::SocketAddr;

use serde_json::{Value, json};
use sqlx::AssertSqlSafe;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use sync_ws_socket_support::SyncFixture;
use ws_credential_support::{mint_coordinator_jwt, now_secs};

const SESSION_ID: &str = "00000000-0000-4000-8000-000000000103";
const NEVER_SET_SESSION_ID: &str = "00000000-0000-4000-8000-000000000104";
const CLOSED_SESSION_ID: &str = "00000000-0000-4000-8000-000000000105";
const FOREIGN_SESSION_ID: &str = "00000000-0000-4000-8000-000000000106";
const SESSIONS_LIST: &str = "/roost.v1.CoordinatorService/SessionsList";
const PRIVATE_REFERENCE_VALUE: &str = "/tmp/private worker/'$conversation.json";

struct ListFixture {
    stack: SyncFixture,
    worker_fp: String,
    worker_token: String,
    device_fp: String,
    device_token: String,
}

impl ListFixture {
    async fn start(label: &str) -> Self {
        let stack = SyncFixture::start(label).await;
        let now = now_secs();
        let (worker_fp, public_key, worker_token) = mint_coordinator_jwt([7; 32], now, now + 300);
        let (foreign_fp, _, _) = mint_coordinator_jwt([8; 32], now, now + 300);
        let (device_fp, device_token) = stack.enroll_browser(9).await;
        let dashboard = scalar_text(&stack, "SELECT id FROM dashboards LIMIT 1").await;
        exec(
            &stack,
            &format!(
                "INSERT INTO authorized_keys (fingerprint, public_key, label, added_at) \
                 VALUES ('{worker_fp}', x'{}', 'test worker', 1000)",
                hex::encode(public_key)
            ),
        )
        .await;
        for fp in [&worker_fp, &foreign_fp] {
            exec(
                &stack,
                &format!(
                    "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
                     VALUES ('{fp}', 'test worker', 'linux', 0, 0, '{dashboard}')"
                ),
            )
            .await;
        }
        let reference = json!({
            "schema_version": 1, "agent_id": "omp", "kind": "path", "value": PRIVATE_REFERENCE_VALUE,
        })
        .to_string()
        .replace('\'', "''");
        for (id, fp, status, reference, seq) in [
            (
                SESSION_ID,
                &worker_fp,
                "open",
                format!("'{reference}'"),
                "17",
            ),
            (
                NEVER_SET_SESSION_ID,
                &worker_fp,
                "open",
                "NULL".to_owned(),
                "NULL",
            ),
            (
                CLOSED_SESSION_ID,
                &worker_fp,
                "closed",
                "NULL".to_owned(),
                "NULL",
            ),
            (
                FOREIGN_SESSION_ID,
                &foreign_fp,
                "open",
                "NULL".to_owned(),
                "NULL",
            ),
        ] {
            exec(
                &stack,
                &format!(
                    "INSERT INTO sessions (id, dashboard_id, worker_fp, channel, kind, cwd, status, \
                     created_at, spawn_cwd, agent_reference_json, agent_reference_client_seq) \
                     VALUES ('{id}', '{dashboard}', '{fp}', 7, 'shell', '/tmp/w', '{status}', 0, \
                     '/tmp/w', {reference}, {seq})"
                ),
            )
            .await;
        }
        Self {
            stack,
            worker_fp,
            worker_token,
            device_fp,
            device_token,
        }
    }

    async fn list(&self, token: &str, body: Value) -> (u16, String) {
        post_json(self.stack.address, SESSIONS_LIST, token, &body.to_string()).await
    }
}

// v2: "worker JWT lists only its own open sessions through coord.fetch".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_lists_only_its_own_open_sessions_with_their_recovery_rows() {
    let fixture = ListFixture::start("list-worker").await;
    let (status, raw) = fixture
        .list(
            &fixture.worker_token,
            json!({ "workerFp": fixture.worker_fp, "status": "open" }),
        )
        .await;
    assert_eq!(status, 200, "{raw}");
    let body: Value = serde_json::from_str(&raw).unwrap();
    let mut ids: Vec<&str> = body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| session["id"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, [SESSION_ID, NEVER_SET_SESSION_ID]);
    assert!(body.get("syncSnapshotToken").is_none());
    let recovery = body["recoveryMetadata"].as_array().unwrap();
    assert_eq!(recovery.len(), 2);
    let set = recovery
        .iter()
        .find(|row| row["sessionId"] == SESSION_ID)
        .unwrap();
    assert_eq!(
        set,
        &json!({
            "sessionId": SESSION_ID,
            "agentReference": {
                "schemaVersion": 1, "agentId": "omp", "kind": "path", "value": PRIVATE_REFERENCE_VALUE,
            },
            "agentReferenceClientSeq": "17",
        })
    );
    let never = recovery
        .iter()
        .find(|row| row["sessionId"] == NEVER_SET_SESSION_ID)
        .unwrap();
    assert_eq!(never, &json!({ "sessionId": NEVER_SET_SESSION_ID }));
    assert!(!body["sessions"].to_string().contains("private worker"));
}

// v2: "worker JWT cannot broaden its session-list scope".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_cannot_broaden_its_list() {
    let fixture = ListFixture::start("list-broaden").await;
    let fp = fixture.worker_fp.clone();
    for body in [
        json!({ "status": "open" }),
        json!({ "workerFp": fp, "status": "all" }),
        json!({ "workerFp": fp, "status": "" }),
        json!({ "workerFp": fp }),
        json!({ "workerFp": fp, "status": "open", "syncSocketId": "browser" }),
        json!({ "workerFp": fp, "status": "open", "syncSocketId": "" }),
        json!({ "workerFp": "ff".repeat(32), "status": "open" }),
    ] {
        let (status, raw) = fixture.list(&fixture.worker_token, body.clone()).await;
        assert_eq!(status, 403, "{body} answered {raw}");
    }
}

// v2: "a browser device lists the sessions but never their recovery metadata",
// and coord-bidi "SessionsList authenticated → 200 + sessions array".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_browser_lists_sessions_but_never_their_recovery_metadata() {
    let fixture = ListFixture::start("list-browser").await;
    let (status, raw) = fixture
        .list(
            &fixture.device_token,
            json!({ "workerFp": fixture.worker_fp, "status": "open" }),
        )
        .await;
    assert_eq!(status, 200, "{raw}");
    let body: Value = serde_json::from_str(&raw).unwrap();
    let mut ids: Vec<&str> = body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| session["id"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, [SESSION_ID, NEVER_SET_SESSION_ID]);
    assert!(body.get("recoveryMetadata").is_none());
    // Worker-recovery state must be absent from the ENTIRE browser response.
    assert!(!raw.contains("private worker"));

    let (status, raw) = fixture
        .list(&fixture.device_token, json!({ "status": "all" }))
        .await;
    assert_eq!(status, 200, "{raw}");
    let every: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(every["sessions"].as_array().unwrap().len(), 4);

    let (status, raw) = fixture
        .list(&fixture.device_token, json!({ "status": "bogus" }))
        .await;
    assert_eq!(status, 400, "{raw}");
    assert!(
        raw.contains("invalid session status \\\"bogus\\\""),
        "{raw}"
    );
}

// v2 `bindSyncSessionSnapshot`: a list naming the caller's own live Sync socket
// is bound to it; a socket the caller does not hold gets no token.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_browser_list_binds_a_snapshot_only_to_its_own_sync_socket() {
    let fixture = ListFixture::start("list-snapshot").await;
    let feed = &fixture.stack.services.feed;
    feed.register_sync_socket("socket-own", &fixture.device_fp);
    feed.register_sync_socket("socket-foreign", &"cd".repeat(32));
    let (status, raw) = fixture
        .list(
            &fixture.device_token,
            json!({ "syncSocketId": "socket-own" }),
        )
        .await;
    assert_eq!(status, 200, "{raw}");
    let body: Value = serde_json::from_str(&raw).unwrap();
    assert!(
        body["syncSnapshotToken"]
            .as_str()
            .is_some_and(|token| !token.is_empty())
    );
    let (status, raw) = fixture
        .list(
            &fixture.device_token,
            json!({ "syncSocketId": "socket-foreign" }),
        )
        .await;
    assert_eq!(status, 200, "{raw}");
    assert!(
        serde_json::from_str::<Value>(&raw)
            .unwrap()
            .get("syncSnapshotToken")
            .is_none()
    );
}

async fn exec(stack: &SyncFixture, sql: &str) {
    sqlx::query(AssertSqlSafe(sql.to_owned()))
        .execute(stack.services.db.pool())
        .await
        .expect("the statement applies");
}

async fn scalar_text(stack: &SyncFixture, sql: &str) -> String {
    sqlx::query_scalar(AssertSqlSafe(sql.to_owned()))
        .fetch_one(stack.services.db.pool())
        .await
        .expect("the value reads")
}

/// One Connect JSON call over a raw HTTP/1.1 connection: the status and body.
async fn post_json(address: SocketAddr, path: &str, token: &str, body: &str) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nConnect-Protocol-Version: 1\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        address.port(),
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, payload) = text.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    let chunked = head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked");
    (
        status,
        if chunked {
            dechunk(payload)
        } else {
            payload.to_owned()
        },
    )
}

fn dechunk(mut payload: &str) -> String {
    let mut body = String::new();
    while let Some((size, rest)) = payload.split_once("\r\n") {
        let size = usize::from_str_radix(size.trim(), 16).unwrap();
        if size == 0 {
            break;
        }
        body.push_str(&rest[..size]);
        payload = &rest[size + 2..];
    }
    body
}
