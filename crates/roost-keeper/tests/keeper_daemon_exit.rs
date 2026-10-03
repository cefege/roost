//! What stops the keeper without a worker asking: SIGTERM, whether the keeper
//! is waiting for a worker or serving one, and its socket file being deleted.
//! Ports the `SIGTERM` handler and the 30 s socket check of v2
//! `apps/worker/src/keeper/multiplexed-main.ts`. A keeper that survives either
//! holds its PTYs, and every shell in them, until the machine reboots — which
//! is how one oracle session left 1155 orphaned keepers behind.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::Duration;

use roost_keeper::codec::MuxFrameType;
use roost_keeper::frames::SpawnAck;
use roost_keeper::server::{Accepted, ConnectionEnd, Endpoint, ExitCause, ExitWatch, Server};
use support::daemon::{Client, DEADLINE, Keeper, TempDir, wait_until};
use support::{idle, spawn_with};

/// `Keeper::start` proves the daemon is listening by connecting once, and the
/// daemon serves that probe until it sees the hangup. Waiting this long puts a
/// signal sent afterwards on a keeper back in its accept wait.
const SETTLE: Duration = Duration::from_millis(300);

/// The socket check interval these tests run with, in place of the daemon's
/// 30 s, so a deleted socket is noticed within the test's patience.
const CHECK: Duration = Duration::from_millis(50);

/// A stop flag no signal ever sets, for the tests about the socket alone. A
/// `static` because the watch takes the flag a signal handler would own.
static NEVER_SIGNALLED: AtomicBool = AtomicBool::new(false);

/// SIGTERM stops a keeper that is waiting for a worker. A worker restarting a
/// degraded keeper and a service manager stopping one both send it.
#[test]
fn sigterm_stops_a_keeper_waiting_for_a_worker() {
    let temp = TempDir::new("exit-waiting");
    let mut keeper = Keeper::start(&temp);
    std::thread::sleep(SETTLE);

    terminate(keeper.pid());
    wait_until("the waiting keeper to exit on SIGTERM", || {
        keeper.has_exited()
    });
    assert!(
        !keeper.socket().exists(),
        "a keeper that stopped removes its socket"
    );
}

/// SIGTERM stops a keeper while a worker is connected, and the stop reaps the
/// shells the keeper holds rather than orphaning them.
#[test]
fn sigterm_stops_a_keeper_serving_a_worker_and_reaps_its_shells() {
    let temp = TempDir::new("exit-serving");
    let mut keeper = Keeper::start(&temp);
    let mut worker = keeper.connect();
    worker.send(&spawn_with(1, idle(), 80, 24));
    let seen = worker.read_until("a spawn ack", |frames| {
        frames
            .iter()
            .any(|frame| frame.frame_type == MuxFrameType::SpawnAck)
    });
    let shell: SpawnAck = seen
        .iter()
        .find(|frame| frame.frame_type == MuxFrameType::SpawnAck)
        .expect("the ack is in what was read")
        .parse_json()
        .expect("the ack decodes");
    assert!(process_exists(shell.pid), "the shell is running");

    terminate(keeper.pid());
    wait_until("the serving keeper to exit on SIGTERM", || {
        keeper.has_exited()
    });
    assert!(
        worker.closed_by_peer(),
        "the worker's connection ends with the keeper"
    );
    wait_until("the keeper's shell to be reaped", || {
        !process_exists(shell.pid)
    });
}

/// A keeper waiting for a worker stops once its socket file is deleted, and
/// not before: a present socket is a keeper a worker can still reach.
#[test]
fn a_deleted_socket_stops_a_keeper_waiting_for_a_worker() {
    let temp = TempDir::new("exit-socket-waiting");
    let socket = temp.socket();
    let mut server = Server::bind(
        Endpoint::new(&socket).expect("an endpoint"),
        temp.capability(),
    )
    .expect("a bind");
    server.watch_exits(ExitWatch::new(&NEVER_SIGNALLED, socket.clone(), CHECK));
    let (report, outcome) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let cause = match server.accept_or_exit() {
            Accepted::Exit(cause) => Some(cause),
            Accepted::Connection(_) => None,
        };
        report.send(cause).expect("the test is listening");
    });

    assert!(
        outcome.recv_timeout(CHECK * 6).is_err(),
        "a keeper whose socket exists keeps waiting"
    );
    std::fs::remove_file(&socket).expect("the socket is deleted");
    assert_eq!(
        outcome.recv_timeout(DEADLINE),
        Ok(Some(ExitCause::SocketRemoved))
    );
    waiter.join().expect("the waiter finished");
}

/// A keeper serving a worker ends that connection once its socket file is
/// deleted, and the daemon reads the end as a reason to stop.
#[test]
fn a_deleted_socket_ends_the_connection_being_served() {
    let temp = TempDir::new("exit-socket-serving");
    let socket = temp.socket();
    let capability = temp.capability();
    let mut server = Server::bind(
        Endpoint::new(&socket).expect("an endpoint"),
        capability.clone(),
    )
    .expect("a bind");
    server.watch_exits(ExitWatch::new(&NEVER_SIGNALLED, socket.clone(), CHECK));
    let (report, outcome) = mpsc::channel();
    let serving = std::thread::spawn(move || {
        let end = match server.accept_or_exit() {
            Accepted::Connection(stream) => Some(server.serve_one(stream)),
            Accepted::Exit(_) => None,
        };
        report.send(end).expect("the test is listening");
    });
    // Authenticated, so what the deletion ends is a SERVED connection rather
    // than one still waiting to prove the capability.
    let _worker = Client::connect(&socket, &capability);

    assert!(
        outcome.recv_timeout(CHECK * 6).is_err(),
        "a keeper whose socket exists keeps serving its worker"
    );
    std::fs::remove_file(&socket).expect("the socket is deleted");
    assert_eq!(
        outcome.recv_timeout(DEADLINE),
        Ok(Some(ConnectionEnd::SocketRemoved))
    );
    serving.join().expect("the server finished");
}

fn terminate(pid: u32) {
    let pid = i32::try_from(pid).expect("a child pid fits a pid_t");
    // SAFETY: `kill` with SIGTERM and the pid of a child this test spawned is
    // the documented use; nothing else is signalled.
    let sent = unsafe { libc::kill(pid, libc::SIGTERM) };
    assert_eq!(sent, 0, "the signal was delivered");
}

fn process_exists(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 delivers nothing; `kill` only reports whether `pid`
    // names a process this test may signal.
    unsafe { libc::kill(pid, 0) == 0 }
}
