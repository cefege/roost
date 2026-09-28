//! Input over a loopback terminal socket reaches a real PTY: the production
//! owner composition (`runtime::owners::WorkerOwners`) builds the local door
//! over a real session layer and a real keeper, a coordinator-shaped grant is
//! installed through the door's grant port, and the bytes a granted Hello's
//! input frame carries are echoed back by the shell. The carrier is a stub.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod keeper_pool_support;
mod local_terminal_support;
mod terminal_stream_support;

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use keeper_pool_support::{KeeperFixture, channel, opened, session, sh_spec};
use local_terminal_support::{GRANT_ID, SECRET, StubPort, TAB, WORKER_EPOCH, WORKER_FP, grant, hello, input};
use roost_proto::__buffa::oneof::local_terminal_server_frame::Frame as ServerFrame;
use roost_protocol::wire::brand::{SessionId, TraceId, WorkerFp};
use roost_term::{AlacrittyCore, CellEmitState};
use roost_worker::event_store::{DurableEventKind, Journal};
use roost_worker::link_ports::LocalTerminalGrantPort;
use roost_worker::runtime::owners::WorkerOwners;
use roost_worker::runtime::session_stack;
use roost_worker::session::ring::ScrollbackRing;
use roost_worker::session::types::{SessionIdentity, SessionRecord};
use roost_worker::uplink;

const SESSION: &str = "00000000-0000-4000-8000-0000000d00e2";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_granted_loopback_socket_writes_to_a_real_pty() {
    let fixture = KeeperFixture::start();
    let pool = fixture.pool();
    let (binding, echo) = session("local-raw-cat");
    let spawned = opened(
        pool.spawn(channel(1), &sh_spec(&["-c", "stty raw -echo; printf ready; exec cat"], &[]), 80, 24, Arc::new(binding)),
        "the keeper opens a real PTY",
    );
    echo.printed("ready");

    let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("roost-local-terminal-pty-{unique}"));
    std::fs::create_dir_all(&root).unwrap();
    let outbox = Arc::new(Journal::open(&root.join("session-events.sqlite")).await.unwrap());
    let stack = session_stack::build(WorkerFp::try_from(WORKER_FP).unwrap(), Arc::clone(&pool), outbox, &root, &root, WORKER_FP.to_owned())
        .expect("this host can build a session layer");
    let reservation = stack.manager.reserve(DurableEventKind::Closed).await.unwrap();
    let identity = SessionIdentity {
        session_id: SessionId::try_from(SESSION).unwrap(),
        channel_id: channel(spawned.channel_id),
        socket_path: "mux:local".to_owned(),
        cwd: root.display().to_string(),
        shell_spec: stack.resolve_shell_spec(&root.display().to_string(), SESSION).unwrap(),
        session_trace_id: TraceId::try_from("0000beef0000beef").unwrap(),
        spawned_at_ms: 0,
    };
    let record = SessionRecord::new(identity, reservation, Box::new(AlacrittyCore::new(80, 24)), CellEmitState::new("epoch", "stream"), ScrollbackRing::default());
    stack.table.insert(record).unwrap();

    let (link, _upstream) = uplink::channel();
    let owners = WorkerOwners::build(stack, &link, WORKER_EPOCH, Arc::clone(&pool), WORKER_FP);
    let door = Arc::clone(&owners.local_terminal);
    door.install_grant(&grant(GRANT_ID, SECRET, &[SESSION], TAB, 60_000)).expect("the coordinator's grant installs");
    let sockets = door.sockets();
    let stub = StubPort::open_on(&sockets, "6a6a6a6a-6a6a-4a6a-8a6a-6a6a6a6a6a6a".to_owned());
    sockets.on_message(stub.as_ref(), &local_terminal_support::encode(hello(GRANT_ID, SECRET, TAB)));
    assert_eq!(stub.cases(), ["ready"]);

    sockets.on_message(stub.as_ref(), &local_terminal_support::encode(input(SESSION, 1, b"local-door-marker")));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !stub.cases().contains(&"inputAccepted") && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let accepted = stub.frames().into_iter().find_map(|frame| match frame {
        ServerFrame::InputAccepted(accepted) => Some(accepted),
        _ => None,
    });
    let accepted = accepted.unwrap_or_else(|| panic!("the keeper acknowledged the batch: {:?}", stub.cases()));
    assert_eq!((accepted.session_id.as_str(), accepted.input_seq, accepted.written_bytes), (SESSION, 1, 17));
    echo.printed("local-door-marker");
    owners.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
