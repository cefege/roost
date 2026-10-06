//! Shared fixtures for the terminal-capture suites: identifiers, the request
//! and evidence builders, a fake worker connection that scripts capture
//! acknowledgements over the real pending-RPC table, and the migrated
//! single-tenant database the bridge resolves its session scope from.
//!
//! Ports `apps/coord/tests/terminal/capture/terminal-capture-harness.ts` and
//! `terminal-capture-evidence.ts`. Shared by `terminal_capture_lease.rs`,
//! `terminal_capture_admission.rs` and `terminal_capture_recorder.rs`.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use connectrpc::ConnectError;
use roost_coord::auth::principal::Principal;
use roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant;
use roost_coord::coord_core::BootFacts;
use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_coord::services::CoordServices;
use roost_coord::terminal_capture::bridge::CaptureBridge;
use roost_coord::terminal_capture::recorder::AdmittedFrameIdentity;
use roost_proto::{TerminalCaptureAction, TerminalCaptureRequest};
use roost_protocol::cell::{CellGridFrame, CellRow, CellSpan};
use roost_protocol::terminal_capture::bundle::TERMINAL_INCIDENT_SCHEMA;
use roost_protocol::terminal_capture::command::TerminalCaptureResult;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use serde_json::{Value, json};

pub const CAPTURE_WORKER: &str = "b2c3d4e5b2c3d4e5b2c3d4e5b2c3d4e5b2c3d4e5b2c3d4e5b2c3d4e5b2c3d4e5";
/// The hub harness's session and stream, so hub-driven records and bridge
/// leases agree.
pub const SESSION_A: &str = "40000000-0000-4000-8000-000000000001";
pub const STREAM: &str = "50000000-0000-4000-8000-000000000001";
pub const EPOCH: &str = "grid-epoch-a";
pub const SESSION_B: &str = "40000000-0000-4000-8000-0000000000b2";
pub const SESSION_C: &str = "40000000-0000-4000-8000-0000000000c3";
pub const RECLAIMED_SESSION: &str = "40000000-0000-4000-8000-0000000000d4";
pub const UNKNOWN_SESSION: &str = "40000000-0000-4000-8000-0000000000ee";
pub const RECORDING_A: &str = "70000000-0000-4000-8000-0000000000a1";
pub const RECORDING_B: &str = "70000000-0000-4000-8000-0000000000b1";
pub const RECORDING_C: &str = "70000000-0000-4000-8000-0000000000c1";
pub const CAPTURE_1: &str = "80000000-0000-4000-8000-000000000001";
pub const CAPTURE_2: &str = "80000000-0000-4000-8000-000000000002";
pub const CAPTURE_3: &str = "80000000-0000-4000-8000-000000000003";
pub const WORKER_CAPTURE_PATH: &str =
    "/home/roost/.roost/logs/terminal-incident-80000000-0000-4000-8000-000000000001.json.gz";
/// Stands in for the terminal text a browser bundle legitimately carries.
pub const EVIDENCE_MARKER: &str = "FOOTER-14s-secret";

/// How the fake worker treats its next capture command.
pub enum CaptureReply {
    Ack(Value),
    /// Take the frame and hold the reply for the test to release.
    Park,
    /// The transport refuses the write.
    Drop,
}

/// Every capture command the fake worker was sent, and the ones it holds.
#[derive(Default)]
pub struct CaptureWorkerLog {
    pub commands: Vec<Value>,
    pub parked: Vec<(String, String)>,
    pub replies: VecDeque<CaptureReply>,
}

pub fn capture_worker_ack(action: &str) -> Value {
    let captured = action == "capture";
    json!({
        "status": match action { "start" => "recording", "stop" => "stopped", _ => "captured" },
        "path": if captured { json!(WORKER_CAPTURE_PATH) } else { Value::Null },
        "byte_length": if captured { json!(4_096) } else { Value::Null },
        "error": null,
        "expires_at_ms": if captured { json!(1_800_000) } else { Value::Null },
        "recent_worker_capture": null,
    })
}

pub struct CaptureFixture {
    pub services: Arc<CoordServices>,
    pub worker: Arc<Mutex<CaptureWorkerLog>>,
    pub handle: Arc<WorkerHandle>,
    pub device_a: Principal,
    pub device_b: Principal,
    root: PathBuf,
}

impl CaptureFixture {
    /// One migrated database with a registered, connected worker, three open
    /// sessions and a fourth the reclaim test closes.
    pub async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-terminal-capture-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let database = super::db_support::open_test_database(&root).await.unwrap();
        let tenant = ensure_self_hosted_tenant(&database, 0).await.unwrap();
        let dashboard_id = tenant.dashboard_id.clone();
        let account_id = tenant.account_id.clone();
        let services = Arc::new(CoordServices::booted(
            database,
            BootFacts {
                tenant: Some(tenant),
                ..BootFacts::unbooted()
            },
        ));
        let worker = Arc::new(Mutex::new(CaptureWorkerLog::default()));
        let handle = connect_worker(&services, &worker);
        let fixture = Self {
            services,
            worker,
            handle,
            device_a: device(&account_id, "device-a"),
            device_b: device(&account_id, "device-b"),
            root,
        };
        fixture
            .exec(&format!(
                "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
                 VALUES ('{CAPTURE_WORKER}', 'capture', 'linux', 1, 1, '{dashboard_id}')"
            ))
            .await;
        for (index, id) in [SESSION_A, SESSION_B, SESSION_C, RECLAIMED_SESSION]
            .iter()
            .enumerate()
        {
            fixture
                .exec(&format!(
                    "INSERT INTO sessions (id, dashboard_id, worker_fp, channel, kind, cwd, status, created_at) \
                     VALUES ('{id}', '{dashboard_id}', '{CAPTURE_WORKER}', {}, 'shell', '/tmp', 'open', 1)",
                    index + 1
                ))
                .await;
        }
        fixture
    }

    pub async fn exec(&self, sql: &str) {
        sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
            .execute(self.services.db.pool())
            .await
            .unwrap();
    }

    pub fn bridge(&self) -> CaptureBridge<'_> {
        CaptureBridge {
            services: &self.services,
            runtime: &self.services.terminal_capture,
            git_sha: "test",
        }
    }

    pub async fn handle(
        &self,
        request: &TerminalCaptureRequest,
        principal: &Principal,
    ) -> Result<TerminalCaptureResult, ConnectError> {
        self.bridge().handle(request, principal).await
    }

    pub fn worker(&self) -> std::sync::MutexGuard<'_, CaptureWorkerLog> {
        self.worker.lock().unwrap()
    }

    /// The `action` of every command the worker received, in order.
    pub fn actions(&self) -> Vec<String> {
        self.worker()
            .commands
            .iter()
            .map(|command| command["action"].as_str().unwrap().to_owned())
            .collect()
    }

    pub fn script(&self, reply: CaptureReply) {
        self.worker().replies.push_back(reply);
    }

    /// Answer a parked command as the worker's `rpc-ok`.
    pub fn release(&self, request_id: &str, data: Value) -> bool {
        self.services
            .scrollback
            .pending()
            .resolve(request_id, data, Some(CAPTURE_WORKER))
    }

    pub fn armed(&self, session_id: &str) -> bool {
        self.services.terminal_capture.recorder.armed(session_id)
    }
}

impl Drop for CaptureFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn device(account_id: &str, fingerprint: &str) -> Principal {
    Principal::AccountDevice {
        fingerprint: fingerprint.to_owned(),
        label: fingerprint.replace('-', " "),
        account_id: account_id.to_owned(),
    }
}

/// A ready worker generation whose socket answers `diag-terminal-capture`
/// with the next scripted reply, or a plain acknowledgement.
fn connect_worker(
    services: &Arc<CoordServices>,
    log: &Arc<Mutex<CaptureWorkerLog>>,
) -> Arc<WorkerHandle> {
    let relay = services.scrollback.clone();
    let log = Arc::clone(log);
    let handle = Arc::new(WorkerHandle::new(
        WorkerFp::try_from(CAPTURE_WORKER).unwrap(),
        None,
        "capture-generation".to_owned(),
        Default::default(),
        Arc::new(move |frame: CoordWorkerDownstream| {
            let CoordWorkerDownstream::BrowserCommand {
                request_id,
                frame:
                    ClientControlFrame::DiagTerminalCapture {
                        recording_id,
                        capture_id,
                        action,
                        browser_evidence_json,
                        coordinator_evidence_json,
                        ..
                    },
                ..
            } = frame
            else {
                panic!("unexpected frame on the capture worker: {}", frame.kind());
            };
            let reply = {
                let mut log = log.lock().unwrap();
                log.commands.push(json!({
                    "kind": "diag-terminal-capture",
                    "action": action,
                    "recording_id": recording_id,
                    "capture_id": capture_id,
                    "browser_evidence_json": browser_evidence_json,
                    "coordinator_evidence_json": coordinator_evidence_json,
                }));
                let reply = log.replies.pop_front();
                if let Some(CaptureReply::Park) = reply {
                    log.parked.push((request_id.clone(), action.clone()));
                }
                reply.unwrap_or_else(|| CaptureReply::Ack(capture_worker_ack(&action)))
            };
            match reply {
                CaptureReply::Drop => 0,
                CaptureReply::Park => 1,
                CaptureReply::Ack(data) => {
                    assert!(
                        relay
                            .pending()
                            .resolve(&request_id, data, Some(CAPTURE_WORKER))
                    );
                    1
                }
            }
        }),
    ));
    assert!(handle.mark_ready());
    services.workers.insert(Arc::clone(&handle));
    handle
}

pub fn request(
    action: TerminalCaptureAction,
    session_id: &str,
    recording_id: &str,
    capture_id: &str,
    browser_evidence_json: &str,
) -> TerminalCaptureRequest {
    TerminalCaptureRequest {
        action: action.into(),
        session_id: session_id.to_owned(),
        recording_id: recording_id.to_owned(),
        capture_id: capture_id.to_owned(),
        reason: "manual".to_owned(),
        browser_evidence_json: browser_evidence_json.to_owned(),
        ..Default::default()
    }
}

/// The browser layer's frozen payload: envelope plus its section NESTED under
/// the `browser` member, the marker standing in for terminal text.
pub fn browser_evidence(capture_id: &str, recording_id: &str, session_id: &str) -> Value {
    json!({
        "schema": TERMINAL_INCIDENT_SCHEMA,
        "layer": "browser",
        "capture_id": capture_id,
        "recording_id": recording_id,
        "session_id": session_id,
        "trigger": {
            "reason": "history_identity", "origin": "browser", "at_ms": 1,
            "stream_id": STREAM, "grid_epoch": EPOCH, "seq": "2",
            "detail": "duplicate_absolute_index", "occurrence_count": 1,
        },
        "browser": {
            "layer": "browser",
            "captured_at_ms": 1,
            "stream": null,
            "geometry": null,
            "dropped": { "records": 0, "bytes": 0, "rows": 0, "raw_bytes": 0, "samples": 0 },
            "omissions": [],
            "events": [],
            "replica": null,
            "current_state": { "dom_viewport": [{ "order": 0, "text": EVIDENCE_MARKER }] },
        },
    })
}

/// A payload whose nested layer section is gone: the exact production defect.
pub fn without_layer_section(mut payload: Value) -> Value {
    let layer = payload["layer"].as_str().unwrap().to_owned();
    payload.as_object_mut().unwrap().remove(&layer);
    payload
}

/// A canonical viewport shaped like the hub's own post-admission frame.
pub fn canonical_frame(
    seq: u64,
    rows: u32,
    spans_per_row: usize,
    text: Option<&str>,
) -> CellGridFrame {
    canonical_frame_in(seq, rows, spans_per_row, text, EPOCH, 80)
}

pub fn canonical_frame_in(
    seq: u64,
    rows: u32,
    spans_per_row: usize,
    text: Option<&str>,
    epoch: &str,
    cols: u32,
) -> CellGridFrame {
    let viewport_rows = (0..rows)
        .map(|index| {
            let text = text.map_or_else(|| format!("row-{index}"), str::to_owned);
            let span = CellSpan {
                columns: text.chars().count() as u32,
                text,
                fg: 256,
                bg: 256,
                flags: 0,
                fg_rgb: None,
                bg_rgb: None,
                link_uri: None,
                link_key: None,
            };
            CellRow {
                index,
                spans: vec![span; spans_per_row].into(),
            }
        })
        .collect();
    CellGridFrame {
        stream_id: STREAM.to_owned(),
        grid_epoch: epoch.to_owned(),
        cols,
        rows,
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: true,
        alt_screen: false,
        cursor_keys_app: false,
        bracketed_paste: false,
        mouse_tracking: Default::default(),
        mouse_sgr: false,
        focus_events: false,
        full: true,
        viewport_rows,
        scrollback_rows: Vec::new(),
        scrollback_append: Vec::new(),
        scrollback_total: 0,
        sb_base: 0,
        base_seq: 0,
        seq,
    }
}

pub fn admitted(full: bool, seq: u64, base_seq: u64) -> AdmittedFrameIdentity {
    AdmittedFrameIdentity {
        full,
        seq,
        base_seq,
    }
}
