//! Closing a channel kills the WHOLE process tree it spawned. Ports
//! `apps/worker/tests/keeper-child-reap.test.ts` against the real daemon.
//!
//! A single signal to the session leader leaks two ways, and each test pins
//! one: an interactive shell ignores SIGTERM, and a foreground job and a
//! background job each live in a job-control process group of their own that a
//! single-pid signal never reaches. A `nohup`ed job ignores the hangup too, so
//! only the snapshot SIGKILL sweep reaps it. Real keeper, real PTYs, real
//! `sleep` children, real `pgrep` — the liveness oracle is kill(pid, 0).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::process::Command;
use std::time::{Duration, Instant};

use roost_keeper::client::{KeeperClient, connect};
use roost_keeper::codec::MuxFrameType;
use roost_keeper::frames::ShellSpec;
use support::daemon::{Keeper, TempDir};

/// Distinctive sleep durations, so `pgrep -f` matches only this file's jobs.
const FG_MARK: &str = "sleep 8873310";
const BG_MARK: &str = "sleep 9982210";
const HUP_MARK: &str = "sleep 7761100";

/// Grace (2 s) plus margin, as v2 waits.
const REAPED_WITHIN: Duration = Duration::from_secs(6);

/// SIGKILL anything a previous red run left behind under ONE test's mark.
///
/// Scoped to one mark because the three tests share this binary and run
/// concurrently. A guard that swept all three marks, dropped at each test's
/// end, ended a SIBLING's live job: `a_nohup_job_that_ignores_sighup_is_still_
/// reaped` asserts its job is still running 300 ms after the kill, and the
/// other two tests' cleanup SIGKILLed it in that window. The test then failed
/// on a neighbour's cleanup rather than on the behaviour it names.
fn sweep_previous_run(mark: &str) {
    for pid in pgrep(mark) {
        signal(pid, libc::SIGKILL);
    }
}

fn interactive_bash() -> ShellSpec {
    ShellSpec {
        program: "/bin/bash".into(),
        args: vec!["--norc".into(), "-i".into()],
        env: vec![
            ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("HOME".into(), std::env::temp_dir().display().to_string()),
        ],
        cwd: Some(std::env::temp_dir().display().to_string()),
    }
}

fn pgrep(pattern: &str) -> Vec<i32> {
    let Ok(output) = Command::new("pgrep").args(["-f", pattern]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .filter(|pid| *pid > 1)
        .collect()
}

fn signal(pid: i32, signal: libc::c_int) {
    if pid > 1 {
        // SAFETY: a positive pid other than init names exactly one process.
        unsafe { libc::kill(pid, signal) };
    }
}

fn is_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks existence; the guard keeps it off 0 and -1.
    pid > 1 && unsafe { libc::kill(pid, 0) } == 0
}

fn wait_for(within: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < within {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done()
}

/// Output of one channel, accumulated from the client's event stream.
fn read_until(client: &KeeperClient, channel_id: u16, seen: &mut String, needle: &str) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(8) {
        if let Some(frame) = client.next_event(Duration::from_millis(50))
            && frame.frame_type == MuxFrameType::PtyOut
            && frame.channel_id == channel_id
        {
            seen.push_str(&String::from_utf8_lossy(&frame.payload));
        }
        if seen.contains(needle) {
            return true;
        }
    }
    false
}

/// A live interactive shell on `channel_id`, at its prompt.
fn shell(client: &KeeperClient, channel_id: u16) -> (i32, String) {
    let pid = client
        .spawn(channel_id, interactive_bash(), 200, 50)
        .expect("bash spawns on a PTY");
    let mut seen = String::new();
    assert!(
        read_until(client, channel_id, &mut seen, "$ "),
        "bash never prompted: {seen:?}"
    );
    (i32::try_from(pid).unwrap(), seen)
}

#[test]
fn the_pty_owns_a_foreground_group_and_resize_delivers_sigwinch() {
    let temp = TempDir::new("reap-winch");
    let keeper = Keeper::start(&temp);
    let client = connect(keeper.socket()).expect("a handshake");
    let (shell_pid, mut seen) = shell(&client, 900);
    client
        .write_input(
            900,
            b"bash --norc -c \"trap 'echo WINCH:\\$(stty size)' WINCH; printf 'CHILD_%s\\n' READY; while :; do sleep 1; done\"\n",
        )
        .unwrap();
    assert!(
        read_until(&client, 900, &mut seen, "CHILD_READY"),
        "{seen:?}"
    );
    assert!(!seen.contains("no job control in this shell"), "{seen:?}");

    assert_eq!(
        client.resize(900, 1, 70, 20).applied_geometry(),
        Some((70, 20))
    );
    assert!(
        read_until(&client, 900, &mut seen, "WINCH:20 70"),
        "{seen:?}"
    );

    client.kill(900).unwrap();
    assert!(
        wait_for(REAPED_WITHIN, || !is_alive(shell_pid)),
        "the shell survived its kill"
    );
}

#[test]
fn an_interactive_shell_and_its_foreground_and_background_jobs_all_die() {
    sweep_previous_run(BG_MARK);
    sweep_previous_run(FG_MARK);
    let temp = TempDir::new("reap-jobs");
    let keeper = Keeper::start(&temp);
    let client = connect(keeper.socket()).expect("a handshake");
    let (shell_pid, _) = shell(&client, 901);
    client
        .write_input(901, format!("{BG_MARK} & {FG_MARK}\n").as_bytes())
        .unwrap();
    assert!(
        wait_for(Duration::from_secs(4), || !pgrep(FG_MARK).is_empty()
            && !pgrep(BG_MARK).is_empty()),
        "both jobs must be running before the kill"
    );
    assert!(is_alive(shell_pid));

    client.kill(901).unwrap();

    let all_dead = wait_for(REAPED_WITHIN, || {
        !is_alive(shell_pid) && pgrep(FG_MARK).is_empty() && pgrep(BG_MARK).is_empty()
    });
    assert!(
        all_dead,
        "leaked after kill: shell={} fg={:?} bg={:?}",
        is_alive(shell_pid),
        pgrep(FG_MARK),
        pgrep(BG_MARK)
    );
}

#[test]
fn a_nohup_job_that_ignores_sighup_is_still_reaped() {
    sweep_previous_run(HUP_MARK);
    let temp = TempDir::new("reap-nohup");
    let keeper = Keeper::start(&temp);
    let client = connect(keeper.socket()).expect("a handshake");
    let (_shell_pid, _) = shell(&client, 902);
    // nohup sets SIGHUP to SIG_IGN and then execs, so the hangup is a no-op on
    // it; the inherited SIG_IGN for SIGTERM makes the group SIGTERM a no-op as
    // well (whichever group job control put it in), so only the snapshot
    // SIGKILL sweep can end it — v2's "escalation SIGKILL path".
    // `$!` is the subshell's pid and `exec` keeps it, so the pid this writes
    // IS the nohuped job. A pattern match is not an identity: the three tests
    // in this bin reap their own trees concurrently, and `pgrep -f` also
    // counts the window between the fork and the exec.
    let pid_file = temp.socket().with_extension("pid");
    client
        .write_input(
            902,
            format!(
                "(trap '' TERM; exec nohup {HUP_MARK}) >/dev/null 2>&1 & echo $! > {}\n",
                pid_file.display()
            )
            .as_bytes(),
        )
        .unwrap();
    let started = Instant::now();
    let job = loop {
        let read = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok());
        if let Some(job) = read.filter(|pid| is_alive(*pid)) {
            break job;
        }
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "the nohup job started: {}",
            pid_file.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    };

    client.kill(902).unwrap();

    // The kill is acknowledged only after the hangup and the group SIGTERM
    // have been delivered, so the job must still be running here: whatever
    // ends it afterwards is the snapshot SIGKILL sweep and nothing else.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        is_alive(job),
        "the graceful signals ended the nohup job {job}"
    );

    assert!(
        wait_for(REAPED_WITHIN, || !is_alive(job)),
        "the nohup job {job} survived the kill"
    );
}
