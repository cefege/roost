//! A coordinator `input-request` through the production input owner, the real
//! session layer and a real keeper, to a real PTY whose echo proves the bytes
//! arrived. Also ports the keeper-key half of
//! `apps/worker/tests/terminal/terminal-stream-input.test.ts`: two browser
//! batches that both say `input_seq = 1` are correlated by worker-owned keeper
//! sequences, so neither answer is lost or swapped.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod keeper_pool_support;

use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use keeper_pool_support::{KeeperFixture, channel, opened, session, sh_spec};
use roost_proto::DInputRequest;
use roost_protocol::wire::brand::{SessionId, TraceId, WorkerFp};
use roost_protocol::wire::coord_worker::{InputResult, TerminalInputStatus, TerminalWritePhase};
use roost_term::{AlacrittyCore, CellEmitState};
use roost_worker::event_store::{DurableEventKind, Journal};
use roost_worker::link_ports::TerminalInputPort;
use roost_worker::runtime::session_stack;
use roost_worker::session::ring::ScrollbackRing;
use roost_worker::session::types::{SessionIdentity, SessionRecord};
use roost_worker::terminal_input::{InputOwner, TerminalInputRouteOwner, TerminalInputWorkBudget};
use roost_worker::uplink::{self, RequestBudget};

const FINGERPRINT: &str = "000000000000000000000000000000000000000000000000000000000000f00d";
const SESSION: &str = "00000000-0000-4000-8000-00000000e2e0";

fn request(input_seq: u64, request_id: &str, data: &[u8]) -> DInputRequest {
    DInputRequest {
        request_id: request_id.to_owned(),
        session_id: SESSION.to_owned(),
        input_seq,
        data: data.to_vec(),
        budget_ms: 5_000,
        ..DInputRequest::default()
    }
}

fn accepted(result: Option<InputResult>, written_bytes: u32) {
    let result = result.expect("the session id is a uuid, so a result is shaped");
    assert_eq!(result.status, TerminalInputStatus::Accepted, "{result:?}");
    assert_eq!(result.phase, TerminalWritePhase::Written);
    assert_eq!(result.written_bytes, written_bytes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_input_request_reaches_a_real_pty_and_equal_browser_sequences_both_land() {
    let fixture = KeeperFixture::start();
    let pool = fixture.pool();
    let (binding, echo) = session("raw-cat");
    let spawned = opened(
        pool.spawn(
            channel(1),
            &sh_spec(&["-c", "stty raw -echo; printf ready; exec cat"], &[]),
            80,
            24,
            Arc::new(binding),
        ),
        "the keeper opens a real PTY",
    );
    echo.printed("ready");

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("roost-input-e2e-{unique}"));
    std::fs::create_dir_all(&root).unwrap();
    let outbox = Arc::new(
        Journal::open(&root.join("session-events.sqlite"))
            .await
            .unwrap(),
    );
    let stack = session_stack::build(
        WorkerFp::try_from(FINGERPRINT).unwrap(),
        Arc::clone(&pool),
        outbox,
        &root,
        &root,
        FINGERPRINT.to_owned(),
        "terminal-input-epoch",
        &roost_worker::agents::environment::AgentReportSite {
            data_dir: root.clone(),
            configured: None,
        },
    )
    .expect("this host can build a session layer");
    let reservation = stack
        .manager
        .reserve(DurableEventKind::Closed)
        .await
        .unwrap();
    let identity = SessionIdentity {
        session_id: SessionId::try_from(SESSION).unwrap(),
        channel_id: channel(spawned.channel_id),
        socket_path: "mux:e2e".to_owned(),
        cwd: root.display().to_string(),
        shell_spec: stack
            .resolve_shell_spec(&root.display().to_string(), SESSION)
            .unwrap(),
        session_trace_id: TraceId::try_from("0000beef0000beef").unwrap(),
        spawned_at_ms: 0,
    };
    let record = SessionRecord::new(
        identity,
        reservation,
        Box::new(AlacrittyCore::new(80, 24)),
        CellEmitState::new("epoch", "stream"),
        ScrollbackRing::default(),
    );
    stack.table.insert(record).unwrap();

    let budget = TerminalInputWorkBudget::new();
    let routes = TerminalInputRouteOwner::new(
        "worker-epoch".to_owned(),
        Arc::clone(&stack.table),
        Arc::clone(stack.manager.control_lanes()),
        budget.clone(),
    );
    let owner = InputOwner::new(
        Arc::clone(&stack.manager),
        Arc::clone(&stack.table),
        routes,
        budget,
    );
    let (link, _receiver) = uplink::channel();
    let budget_now = || RequestBudget::from_budget_ms(5_000, Instant::now());

    accepted(
        owner
            .write_input(
                request(1, "first", b"e2e-marker"),
                budget_now(),
                link.fence(),
            )
            .await,
        10,
    );
    echo.printed("e2e-marker");

    let (left, right) = tokio::join!(
        owner.write_input(request(1, "left", b"<left>"), budget_now(), link.fence()),
        owner.write_input(request(1, "right", b"<right>"), budget_now(), link.fence()),
    );
    accepted(left, 6);
    accepted(right, 7);
    let text = echo.printed("<right>");
    assert!(
        text.contains("<left>"),
        "both batches reached the PTY: {text:?}"
    );
    assert_eq!(
        pool.pending_input(spawned.channel_id).commands,
        0,
        "every keeper sequence settled"
    );
    let _ = std::fs::remove_dir_all(&root);
}
