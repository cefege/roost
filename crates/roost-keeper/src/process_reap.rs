//! Process-tree reaping for a keeper channel: closing a pane must end EVERY
//! process it spawned. Ports the POSIX half of v2
//! `apps/worker/src/keeper/keeper-process-reap.ts`. Called by
//! `pty_channel::PtyChannel::kill` (a `KillChild`, a respawn over a live
//! channel) and by `keeper::Keeper::reap_all_channels` before the daemon exits.
//!
//! A single signal to the leader leaks, two ways: an interactive shell sets
//! SIGTERM to SIG_IGN, and its foreground and background jobs live in
//! job-control process groups a single-pid signal never reaches. So the tree is
//! snapshotted BEFORE anything is signalled — a child whose parent dies is
//! reparented and the link is lost — then the terminal is hung up, the leader's
//! group gets SIGTERM, and every survivor of the snapshot gets SIGKILL after a
//! grace interval.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Grace between the graceful reap signals (hangup + group SIGTERM) and the
/// SIGKILL sweep of survivors.
pub const REAP_GRACE: Duration = Duration::from_millis(2000);

/// How long the process listing may take. A `ps` that hangs must not hang the
/// keeper's one serving thread.
const PS_TIMEOUT: Duration = Duration::from_millis(2000);

/// The processes one channel's reap starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReapTarget {
    /// The child the PTY was opened for: a session leader, whose pid is also
    /// its process group id.
    pub leader: i32,
    /// The terminal's foreground process group, when the tty reports one.
    pub foreground_group: Option<i32>,
}

/// One snapshot member and its birth, so the deferred SIGKILL can prove it is
/// killing the process it saw rather than a recycled pid.
#[derive(Debug, Clone, Copy)]
struct Member {
    pid: i32,
    birth: Option<u64>,
}

/// Graceful reap of one channel's whole tree, then the SIGKILL escalation on a
/// thread of its own after [`REAP_GRACE`].
pub fn reap_channel_tree(target: ReapTarget) {
    let tree = collect_process_tree(target.leader);
    hang_up(target);
    signal_group(target.leader, libc::SIGTERM);
    // Births are read after the group SIGTERM, which is where v2 reads them.
    let members: Vec<Member> = tree
        .iter()
        .map(|&pid| Member {
            pid,
            birth: start_time_ticks(pid),
        })
        .collect();
    tracing::info!(
        leader = target.leader,
        foreground_group = ?target.foreground_group,
        members = members.len(),
        "keeper: a channel's process tree was signalled; survivors are killed after the grace"
    );
    let sweep = std::thread::Builder::new()
        .name("roost-keeper-reap".into())
        .spawn(move || {
            std::thread::sleep(REAP_GRACE);
            sweep_survivors(&members);
        });
    if let Err(error) = sweep {
        tracing::error!(%error, leader = target.leader, "keeper: the reap sweep could not start");
    }
}

/// Reap every channel before the keeper exits: hang each terminal up and
/// SIGKILL every live member of every tree at once (v2 `reapAllChannels`).
pub fn reap_all_channels(targets: &[ReapTarget]) {
    let mut victims = Vec::new();
    for &target in targets {
        victims.extend(collect_process_tree(target.leader));
        hang_up(target);
    }
    let mut killed = 0_usize;
    for pid in victims {
        if is_process_alive(pid) && send(pid, libc::SIGKILL) {
            killed += 1;
        }
    }
    tracing::info!(
        channels = targets.len(),
        killed,
        "keeper: every channel's process tree was reaped before exit"
    );
}

/// SIGTERM one process, by pid: the worker's graceful stop of a keeper it
/// started. True when the signal was delivered. A pid `kill` would read as a
/// group or a broadcast (0, 1, anything past `i32::MAX`) is refused.
pub fn terminate_process(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    let delivered = send(pid, libc::SIGTERM);
    tracing::info!(pid, delivered, "a process was asked to terminate");
    delivered
}

/// The kernel's hangup, delivered by hand. v2 closed the PTY master and the tty
/// sent SIGHUP; this keeper's reader and input threads hold duplicates of the
/// master, so dropping one descriptor hangs nothing up, and the same signals go
/// to the same processes instead: the session leader and the foreground group.
fn hang_up(target: ReapTarget) {
    if let Some(group) = target.foreground_group
        && group != target.leader
    {
        signal_group(group, libc::SIGHUP);
    }
    if target.leader > 1 {
        send(target.leader, libc::SIGHUP);
        send(target.leader, libc::SIGCONT);
    }
}

/// SIGKILL every snapshot member still alive, skipping only a pid /proc PROVES
/// is not the one snapshotted. An unreadable snapshot birth keeps the kill: an
/// unverifiable member must not weaken the every-survivor-dies guarantee.
fn sweep_survivors(members: &[Member]) {
    let mut killed = 0_usize;
    for member in members {
        if !is_process_alive(member.pid) {
            continue;
        }
        if VERIFIES_BIRTHS && member.birth.is_some() && start_time_ticks(member.pid) != member.birth
        {
            continue;
        }
        if send(member.pid, libc::SIGKILL) {
            killed += 1;
        }
    }
    tracing::info!(
        members = members.len(),
        killed,
        "keeper: the reap sweep killed the tree's survivors"
    );
}

/// One process listing → every descendant of `root`, root included.
fn collect_process_tree(root: i32) -> Vec<i32> {
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    for line in process_listing().lines() {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(ppid), None) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid)) = (pid.parse::<i32>(), ppid.parse::<i32>()) else {
            continue;
        };
        children.entry(ppid).or_default().push(pid);
    }
    let mut tree = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        tree.push(pid);
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids);
        }
    }
    tree
}

/// `ps -A -o pid=,ppid=`, bounded by [`PS_TIMEOUT`]. Empty when `ps` is missing
/// or fails, which leaves the tree as the leader alone, as v2 did.
fn process_listing() -> String {
    let spawned = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid="])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(%error, "keeper: ps could not run; the reap sees only the leader");
            return String::new();
        }
    };
    // Read on a thread: a listing larger than the pipe buffer would otherwise
    // block `ps` on a full pipe while this thread waited for it to exit.
    let reader = child.stdout.take().map(|mut stdout| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stdout.read_to_string(&mut text);
            text
        })
    });
    let deadline = Instant::now() + PS_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                tracing::warn!("keeper: ps did not finish in time; the reap uses what it printed");
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default()
}

/// Whether the deferred sweep can prove a pid's identity (Linux /proc).
const VERIFIES_BIRTHS: bool = cfg!(target_os = "linux");

/// /proc/<pid>/stat field 22: the start time in clock ticks since boot, stable
/// for a process's whole life and different after the pid is recycled. `None`
/// when unreadable (vanished, or not Linux).
fn start_time_ticks(pid: i32) -> Option<u64> {
    if !VERIFIES_BIRTHS {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `comm` can contain spaces and parentheses: resume after the LAST ')'.
    // Field 3 (state) is then index 0, so field 22 is index 19.
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    rest.split(' ').nth(19)?.parse().ok()
}

/// kill(pid, 0) liveness; self and pid <= 1 are never alive to this module.
fn is_process_alive(pid: i32) -> bool {
    if pid <= 1 || u32::try_from(pid).ok() == Some(std::process::id()) {
        return false;
    }
    // SAFETY: signal 0 performs only the permission and existence check; the
    // guard above keeps it off pid 0, -1 and this process.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Signal one process. True when it was delivered.
fn send(pid: i32, signal: libc::c_int) -> bool {
    if pid <= 1 || u32::try_from(pid).ok() == Some(std::process::id()) {
        return false;
    }
    // SAFETY: a positive pid other than init and this process names exactly
    // one process; kill(0) and kill(-1) would reach every process we may signal.
    unsafe { libc::kill(pid, signal) == 0 }
}

/// Signal a whole process group. Refused for group ids that `kill` would read
/// as "my group" or "everyone".
fn signal_group(group: i32, signal: libc::c_int) -> bool {
    if group <= 1 {
        return false;
    }
    // SAFETY: a negative operand names exactly the group `group`; the guard
    // above keeps it off kill(0) (our own group) and kill(-1) (broadcast).
    unsafe { libc::kill(-group, signal) == 0 }
}
