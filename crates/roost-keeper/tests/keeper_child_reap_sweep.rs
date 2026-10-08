#![cfg(unix)]
//! The snapshot SIGKILL sweep, on its own. Ports the `nohup` half of
//! `apps/worker/tests/keeper-child-reap.test.ts` (`keeper-process-reap.ts`'s
//! escalation path) against the real daemon, and it is a separate binary from
//! `keeper_child_reap.rs` on purpose.
//!
//! A single signal to the session leader leaks: an interactive shell sets
//! SIGTERM to SIG_IGN, and its foreground and background jobs live in
//! job-control process groups a single-pid signal never reaches. So the tree is
//! snapshotted BEFORE anything is signalled, the terminal is hung up, the
//! leader's group gets SIGTERM, and every survivor of the snapshot gets SIGKILL
//! after a grace interval.
//!
//! The sweep is only the mechanism that can end a `nohup`ed job, and proving
//! that needs this test to be the only one on the host. Its siblings reap
//! their own trees concurrently, and a job this test's own daemon never
//! SIGKILLs can still be gone by the time the budget runs out: with the sweep
//! removed (`sweep_survivors(&members);` -> `drop(members);`) the guard passes
//! in the three-test binary and fails in 6.59 s here, alone. A guard whose
//! verdict depends on which tests happen to share the machine is not a guard,
//! so this one runs by itself.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::time::{Duration, Instant};

use roost_keeper::client::{KeeperClient, connect};
use roost_keeper::frames::ShellSpec;
use support::daemon::{Keeper, TempDir};

/// A distinctive sleep duration, so the pid read back from the shell is
/// corroborated by `pgrep` before the test trusts it.
const HUP_MARK: &str = "sleep 7761100";

/// Grace (2 s) plus margin, as v2 waits.
const REAPED_WITHIN: Duration = Duration::from_secs(6);

/// SIGKILL whatever a previous red run of this test left behind under its mark.
fn sweep_previous_run() {
    for pid in pgrep(HUP_MARK) {
        // SAFETY: a SIGKILL to a pid `pgrep` just matched, which it keeps off 0 and 1.
        unsafe { libc::kill(pid, libc::SIGKILL) };
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

fn is_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks existence; the guard keeps it off 0 and -1.
    pid > 1 && unsafe { libc::kill(pid, 0) } == 0
}

fn pgrep(pattern: &str) -> Vec<i32> {
    let Ok(output) = std::process::Command::new("pgrep")
        .args(["-f", pattern])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .filter(|pid| *pid > 1)
        .collect()
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
            && frame.frame_type == roost_keeper::codec::MuxFrameType::PtyOut
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

fn shell(client: &KeeperClient, channel_id: u16) -> i32 {
    let pid = client
        .spawn(channel_id, interactive_bash(), 200, 50)
        .expect("bash spawns on a PTY");
    let mut seen = String::new();
    assert!(
        read_until(client, channel_id, &mut seen, "$ "),
        "bash never prompted: {seen:?}"
    );
    i32::try_from(pid).unwrap()
}

#[test]
fn a_nohup_job_that_ignores_sighup_is_still_reaped() {
    sweep_previous_run();
    let temp = TempDir::new("reap-sweep");
    let keeper = Keeper::start(&temp);
    let client = connect(&keeper.endpoint()).expect("a handshake");
    let _shell_pid = shell(&client, 902);
    // nohup sets SIGHUP to SIG_IGN and then execs, so the hangup is a no-op on
    // it; the inherited SIG_IGN for SIGTERM makes the group SIGTERM a no-op as
    // well, so only the snapshot SIGKILL sweep can end it — v2's
    // "escalation SIGKILL path".
    //
    // `$!` is the subshell's pid and `exec` keeps it, so the pid the shell
    // writes IS the nohuped job: a pattern match is not an identity, and
    // pgrep counts the window between the fork and the exec.
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
        if let Some(job) = read.filter(|pid| is_alive(*pid) && pgrep(HUP_MARK).contains(pid)) {
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

    assert!(
        wait_for(REAPED_WITHIN, || !is_alive(job)),
        "the nohup job {job} survived the kill: the snapshot sweep is what ends it"
    );
    // The TempDir removes itself; the job is gone, so nothing is left behind.
    drop(keeper);
}
