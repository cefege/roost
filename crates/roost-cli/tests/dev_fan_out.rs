//! The fan-out, against real child processes. `roost dev` starts a
//! coordinator, a worker and a web dev server and has to leave none of them
//! behind; the children here are shells rather than those three servers, so the
//! signal path is exercised for what it is rather than for what a coordinator
//! happens to do with it.
//!
//! Each child reports its own pid and, when the signal it was sent arrives,
//! writes a marker from inside its own handler. A reaped pid cannot tell those
//! two apart — SIGKILL also reaps — so the marker is what proves the signal was
//! delivered and handled. It also rewrites a counter on every pass, which is
//! what a test that has to decide "is it still running?" watches: a reaped pid
//! is indistinguishable from a live one to `kill`, but not to a moving file.

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
/// How long `assert_stopped` watches the beat counter. Six pass periods, so a
/// child that outlived the order to stop gets several chances to move it.
const SETTLE: Duration = Duration::from_millis(300);
/// The period the sleeper's beat counter advances at. A literal rather than a
/// formatted `Duration`, because this string is handed to whichever `sleep` is
/// installed and only the fraction form is portable.
const PASS_PERIOD: &str = "0.05";

fn policy() -> StopPolicy {
    StopPolicy { grace: GRACE }
}

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roost-dev-fanout-{}-{label}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// A shell that reports its pid, then waits, rewriting its beat counter on
/// every pass. With `ignore` set it discards the polite signal, which is the
/// only way to see the escalation.
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
        "{handler}\necho $$ > {}\ncount=0\nwhile :; do count=$((count+1)); echo $count > {}; sleep {PASS_PERIOD}; done",
        pid_file(dir, name).display(),
        beat_file(dir, name).display()
    );
    DevServer::new(name, "/bin/sh", &["-c", &script])
}

fn beat_file(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.beat"))
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

/// The counter's current value, or `None` when the child never wrote one.
/// Those are different facts, and the helpers below refuse to trade one for
/// the other: a file that was never created is a child nobody watched, which
/// is not the same claim as a file that stopped moving.
fn beat_now(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(&beat_file(dir, name)).ok()
}

/// The counter moved while it was watched. This is the only way a test can
/// know it is looking at a live child rather than a pid the kernel has
/// already handed to somebody else, and it is what lets a caller stand on a
/// beat file that is known to exist.
fn assert_running(dir: &Path, name: &str) {
    let first = beat_now(dir, name).unwrap_or_default();
    let deadline = Instant::now() + DEADLINE;
    while beat_now(dir, name).unwrap_or_default() == first {
        assert!(
            Instant::now() < deadline,
            "{name} never proved it was still running"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The child beat, and then it stopped. A child that outlived the order to
/// stop keeps rewriting the counter for as long as the window lasts, so a
/// stack that quietly lost a third of itself cannot pass this — and a file
/// that was never written is refused rather than read as stillness.
fn assert_stopped(dir: &Path, name: &str) {
    let first = beat_now(dir, name).unwrap_or_else(|| {
        panic!("{name} never wrote a beat, so calling it stopped would be vacuous")
    });
    std::thread::sleep(SETTLE);
    let second = beat_now(dir, name).unwrap_or_default();
    assert_eq!(
        first, second,
        "{name} kept running after it was told to stop"
    );
}

/// The counter is not advancing, whether or not it ever was.
///
/// The weak half of the pair, and it exists because the strict half is not
/// available at every call site. `DevStack::start` signals what it started
/// within a millisecond of spawning it, so a shell that has not yet passed its
/// first line dies to the default disposition and writes no beat file. That is
/// the SAME event that decides whether the trap marker exists, so demanding the
/// file here would reinstate the race this pair was split to remove.
///
/// What it still catches is the regression it is here for: a child that
/// outlives the refusal keeps beating, and the comparison fails. The one case
/// it cannot see is a child killed before its first tick, which is a child
/// that did not outlive anything.
fn assert_no_beat(dir: &Path, name: &str) {
    let first = beat_now(dir, name).unwrap_or_default();
    std::thread::sleep(SETTLE);
    let second = beat_now(dir, name).unwrap_or_default();
    assert_eq!(
        first, second,
        "{name} kept beating after the stack refused to start"
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

/// Delivery of the polite signal to a child that has announced itself.
/// `sleeper` installs its trap before it writes its pid file, so that file
/// appearing IS "ready for a signal" — and this test owns both ends of the
/// exchange, so it can wait there instead of hoping.
#[tokio::test]
async fn a_signal_sent_to_a_child_that_reported_itself_is_handled_by_that_child() {
    let dir = scratch("ready-then-signalled");
    let mut stack = DevStack::start(&[sleeper(&dir, COORDINATOR, false)], policy())
        .await
        .expect("the stack starts");
    await_started(&dir, &[COORDINATOR]);
    // The positive control for `assert_stopped`: a probe that never watched a
    // live child would make "it stopped" mean nothing.
    assert_running(&dir, COORDINATOR);

    stop_within_deadline(&mut stack, TerminationSignal::Interrupt).await;

    assert_handled(&dir, COORDINATOR);
    assert_stopped(&dir, COORDINATOR);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A start that fails halfway: the refusal must name the child it could not
/// start, and the coordinator that DID start must not outlive it.
///
/// Nothing here waits for readiness, and that is the point. `DevStack::start`
/// starts the first child, fails on the missing program and signals what it
/// started inside the same call, so this test has no moment between the two
/// and no way to learn that the shell reached its `trap` line. Asserting
/// delivery from in there therefore races the child's own startup: three
/// full-suite runs hit it once, two isolated re-runs then passed 4/4, and the
/// diff touches no `dev/` product file. The child is watched instead, through
/// a counter it rewrites for as long as it lives, which decides "is it still
/// running?" without claiming to know when it became ready. Delivery has its
/// own test above, which owns both ends and waits for the pid file.
///
/// Widening `GRACE` would make this greener and leave the race exactly where
/// it is, which is why it is not the fix. So is a readiness callback on
/// `start` or a readiness field on `DevServer`: nothing in production waits
/// for readiness, so either would be a concept with no user.
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
    // the coordinator that DID start must not outlive the refusal. This is the
    // weak probe, for the reason on its own doc; there is no point inside
    // `start` at which the strict one could be earned.
    assert_no_beat(&dir, COORDINATOR);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The strict probe must refuse a child it never saw alive. Without this the
/// precondition is only a comment, and the next reader cannot tell a
/// deliberate refusal from a reader that quietly defaults to empty.
#[test]
fn the_strict_probe_refuses_a_child_that_never_beat() {
    let dir = scratch("never-beat");
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let verdict = std::panic::catch_unwind(|| assert_stopped(&dir, "no-such-child"));
    std::panic::set_hook(quiet);
    assert!(
        verdict.is_err(),
        "a beat file that was never written was reported as a child that stopped"
    );
    // The weak half tolerates exactly what the strict half refuses, which is
    // the whole reason they are two functions and not one.
    assert_no_beat(&dir, "no-such-child");
    let _ = std::fs::remove_dir_all(&dir);
}
