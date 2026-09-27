//! The fan-out, against real child processes. `roost dev` starts a
//! coordinator, a worker and a web dev server and has to leave none of them
//! behind; the children here are shells rather than those three servers, so the
//! signal path is exercised for what it is rather than for what a coordinator
//! happens to do with it.
//!
//! Each child reports its own pid and, when the signal it was sent arrives,
//! writes a marker from inside its own handler. A reaped pid cannot tell those
//! two apart — SIGKILL also reaps — so the marker is what proves the signal was
//! delivered and handled.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use roost_cli::dev::plan::{COORDINATOR, DevServer, WEB, WORKER};
use roost_cli::dev::supervisor::{DevStack, StopPolicy, TerminationSignal, TerminationWatch};

/// Long enough that a loaded machine is not a failure, short enough that a
/// hang fails instead of stalling the suite.
const DEADLINE: Duration = Duration::from_secs(20);
/// The production grace is five seconds; a test that waits it out proves the
/// same thing in a fraction of the time.
const GRACE: Duration = Duration::from_millis(300);

fn policy() -> StopPolicy {
    StopPolicy { grace: GRACE }
}

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roost-dev-fanout-{}-{label}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// A shell that reports its pid, then waits. With `ignore` set it discards the
/// polite signal, which is the only way to see the escalation.
fn sleeper(dir: &Path, name: &'static str, ignore: bool) -> DevServer {
    let handler = if ignore {
        "trap '' INT TERM".to_string()
    } else {
        format!(
            "trap 'echo handled > {}' INT TERM",
            marker(dir, name).display()
        )
    };
    let script = format!(
        "{handler}\necho $$ > {}\nwhile :; do sleep 0.05; done",
        pid_file(dir, name).display()
    );
    DevServer::new(name, "/bin/sh", &["-c", &script])
}

fn pid_file(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.pid"))
}

fn marker(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.handled"))
}

/// Every named child is a distinct, running process before the test signals
/// anything: a child started after the signal would never receive it, and the
/// test would pass for the wrong reason.
fn await_started(dir: &Path, names: &[&str]) {
    let deadline = Instant::now() + DEADLINE;
    let mut pids = Vec::new();
    for name in names {
        let path = pid_file(dir, name);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "{name} never reported that it started"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let reported = std::fs::read_to_string(&path).expect("pid file");
        pids.push(reported.trim().to_string());
    }
    let mut unique = pids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        pids.len(),
        "two children shared a pid: {pids:?}"
    );
}

fn assert_handled(dir: &Path, name: &str) {
    assert!(
        marker(dir, name).exists(),
        "{name} never reported handling the signal it was sent"
    );
}

async fn stop_within_deadline(stack: &mut DevStack, forwarded: TerminationSignal) {
    let stopped = tokio::time::timeout(DEADLINE, stack.stop(forwarded))
        .await
        .unwrap_or_else(|_| panic!("the dev stack was still running after {DEADLINE:?}"));
    stopped.expect("the fan-out reported a child it could not stop");
}

#[tokio::test]
async fn a_sigint_reaches_every_child_and_none_outlives_the_command() {
    let dir = scratch("signal");
    let mut watch = TerminationWatch::install().expect("signal watch");
    let mut stack = DevStack::start(
        &[
            sleeper(&dir, COORDINATOR, false),
            sleeper(&dir, WORKER, false),
            sleeper(&dir, WEB, false),
        ],
        policy(),
    )
    .await
    .expect("the stack starts");
    await_started(&dir, &[COORDINATOR, WORKER, WEB]);

    let mut next = Box::pin(watch.next());
    // Polled once before the signal exists, because the process handler is what
    // keeps a SIGINT from ending this test binary instead of arriving here.
    tokio::select! {
        _ = &mut next => panic!("no signal has been sent yet"),
        _ = tokio::time::sleep(Duration::from_millis(50)) => {}
    }
    let sent = std::process::Command::new("kill")
        .arg("-INT")
        .arg(std::process::id().to_string())
        .status()
        .expect("the test could not signal itself");
    assert!(sent.success(), "the test could not signal itself");

    let received = tokio::time::timeout(DEADLINE, next)
        .await
        .expect("the watch never saw the signal this process was sent");
    assert_eq!(received, Some(TerminationSignal::Interrupt));
    stop_within_deadline(&mut stack, TerminationSignal::Interrupt).await;

    for name in [COORDINATOR, WORKER, WEB] {
        assert_handled(&dir, name);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_server_that_exits_alone_takes_the_other_two_with_it() {
    let dir = scratch("one-exits");
    let quitter = DevServer::new(WEB, "/bin/sh", &["-c", "exit 3"]);
    let mut stack = DevStack::start(
        &[
            sleeper(&dir, COORDINATOR, false),
            sleeper(&dir, WORKER, false),
            quitter,
        ],
        policy(),
    )
    .await
    .expect("the stack starts");
    await_started(&dir, &[COORDINATOR, WORKER]);

    let exit = tokio::time::timeout(DEADLINE, stack.wait_for_exit())
        .await
        .expect("no dev server exited")
        .expect("no dev server was left running");
    assert_eq!(exit.name, WEB);
    assert_eq!(exit.code, Some(3));
    stop_within_deadline(&mut stack, TerminationSignal::Interrupt).await;

    assert_handled(&dir, COORDINATOR);
    assert_handled(&dir, WORKER);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_child_that_ignores_the_signal_is_killed_after_the_grace() {
    let dir = scratch("stubborn");
    let mut stack = DevStack::start(&[sleeper(&dir, COORDINATOR, true)], policy())
        .await
        .expect("the stack starts");
    await_started(&dir, &[COORDINATOR]);

    let stopping = Instant::now();
    stop_within_deadline(&mut stack, TerminationSignal::Interrupt).await;

    // Reported gone before the grace elapsed would mean the child was reaped
    // without ever being asked politely, which is the state this escalation
    // exists to prevent.
    assert!(
        stopping.elapsed() >= GRACE,
        "a child that ignored SIGINT was reported gone after {:?}",
        stopping.elapsed()
    );
    assert!(
        !marker(&dir, COORDINATOR).exists(),
        "a child that discarded the signal cannot have handled it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A start that fails halfway: the coordinator that DID start must not outlive
/// the refusal, and the child must have been ready to receive the signal.
///
/// **THIS TEST HAS A RACE, AND IT IS THE TEST'S, NOT THE MACHINE'S.** Measured:
/// three isolated runs of this target gave one pass, one failure, one pass. The
/// cause is the shape below — `DevStack::start` starts the coordinator, fails
/// on the missing program, and immediately signals the coordinator, and the
/// 300 ms `GRACE` is all that stands between that signal and a shell that has
/// not yet reached its `trap` line. It is a real race, not load: the failing
/// runs were on an idle machine with nothing else contending.
///
/// **THE FIX NEEDS A SEAM IN `DevStack`, SO IT IS NOT LANDED HERE.** The honest
/// version is determinism, and the child already announces itself: `sleeper`
/// writes its pid file AFTER installing the trap, so that file's existence IS
/// "ready for a signal", and `await_started` above proves the mechanism by
/// waiting on it. What is missing is a way to wait there too — the start and
/// the stop both happen inside `DevStack::start`, and this test has no point
/// between them. Closing it means either a readiness callback on `start` (a
/// production API added for a test) or a readiness field on `DevServer` (a test
/// concept inside production data). Both are design changes to a production
/// seam, so they are reported rather than pushed unreviewed.
///
/// Widening `GRACE` would make this greener and leave the race exactly where
/// it is, which is why it is not the fix.
#[tokio::test]
async fn a_server_that_cannot_start_names_itself_and_stops_what_already_ran() {
    let dir = scratch("no-program");
    let missing = DevServer::new(WEB, "roost-no-such-dev-server", &[]);
    let outcome = DevStack::start(
        &[sleeper(&dir, COORDINATOR, false), missing],
        StopPolicy::default(),
    )
    .await;

    let failure = match outcome {
        Ok(_) => panic!("a program that does not exist was started"),
        Err(failure) => failure,
    };
    assert!(
        failure.message.contains("web"),
        "the failure must name the child: {}",
        failure.message
    );
    assert!(
        failure.message.contains("roost-no-such-dev-server"),
        "the failure must name the program: {}",
        failure.message
    );
    // A start that failed halfway is the state the command exists to prevent:
    // the coordinator that DID start must not outlive the refusal.
    assert_handled(&dir, COORDINATOR);
    let _ = std::fs::remove_dir_all(&dir);
}
