// Process-tree reaping for the multiplexed keeper. POSIX retains the PTY
// close/process-group/tree sweep. Windows closes its authenticated job-host
// control channel and waits for ACTIVE_PROCESS_ZERO before releasing ConPTY.

import { readFileSync } from "node:fs";
import type { Channel } from "./keeper-types.ts";
import { assertNeverPlatform, supportedHostPlatform, type SupportedHostPlatform } from "@roost/shared/platform";

const HOST_PLATFORM = supportedHostPlatform() as SupportedHostPlatform;

// Grace between the graceful reap signals (PTY master close + group SIGTERM)
// and the SIGKILL sweep of survivors. A short termination grace interval.
const REAP_GRACE_MS = 2000;

// --- Process-tree reaping (closing a pane must kill EVERYTHING it spawned) ---
// A single `ch.proc.kill("SIGTERM")` leaked: interactive shells set
// SIGTERM->SIG_IGN (no-op), and foreground/background jobs live in their own
// job-control process groups a single-pid signal never reaches. Fix:
// close the PTY master (kernel SIGHUP -> tty foreground group) +
// group SIGTERM, then SIGKILL any survivor from a pre-death `ps` tree snapshot.

/** One `ps` snapshot -> every descendant pid of `rootPid` (root included).
 *  Captured BEFORE signaling: once a child's parent dies it reparents to
 *  launchd (ppid=1) and the tree link is lost. */
function collectProcessTree(rootPid: number): number[] {
  const childrenOf = new Map<number, number[]>();
  try {
    const out = Bun.spawnSync(["ps", "-A", "-o", "pid=,ppid="], { timeout: 2000, killSignal: "SIGKILL" }).stdout.toString();
    for (const line of out.split("\n")) {
      const m = line.trim().match(/^(\d+)\s+(\d+)$/);
      if (!m) continue;
      const pid = Number(m[1]); const ppid = Number(m[2]);
      const kids = childrenOf.get(ppid);
      if (kids) kids.push(pid); else childrenOf.set(ppid, [pid]);
    }
  } catch { /* ps missing/failed -> fall back to just the leader below */ }
  const tree: number[] = [];
  const seen = new Set<number>();
  const stack = [rootPid];
  while (stack.length) {
    const pid = stack.pop()!;
    if (seen.has(pid)) continue;
    seen.add(pid);
    tree.push(pid);
    for (const kid of childrenOf.get(pid) ?? []) stack.push(kid);
  }
  return tree;
}

/** Linux-only identity token: /proc/<pid>/stat field 22, the process start
 *  time in clock ticks since boot — stable for a process's whole life and
 *  different after the pid is recycled. null when unreadable (vanished,
 *  or non-Linux). */
function procStartTimeTicks(pid: number): bigint | null {
  try {
    const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
    // comm can contain spaces/parens: resume AFTER the final ')'. Field 3
    // (state) is then index 0, so field 22 is index 19.
    const fields = stat.slice(stat.lastIndexOf(")") + 2).split(" ");
    return BigInt(fields[19]!);
  } catch {
    return null;
  }
}

/** kill(pid,0) liveness probe; ESRCH => gone. Guards self + pid<=1. */
function isProcessAlive(pid: number): boolean {
  if (pid <= 1 || pid === process.pid) return false;
  try { process.kill(pid, 0); return true; } catch { return false; }
}

function reapPosixChannel(ch: Channel): void {
  const leader = ch.proc.pid;
  const tree = collectProcessTree(leader);
  try { ch.terminal.close(); } catch { /* already closed */ } // SIGHUP -> foreground group
  // Guard: kill(-1) / kill(-0) would signal every reachable process.
  if (leader > 1) try { process.kill(-leader, "SIGTERM"); } catch { /* gone / no group */ }
  // Snapshot each member's birth at collection time so the deferred SIGKILL
  // below can prove it is killing the process it saw, not a recycled pid.
  const births = HOST_PLATFORM === "linux"
    ? new Map(tree.map((pid) => [pid, procStartTimeTicks(pid)]))
    : undefined; // darwin has no /proc; accepted residual risk (2s window)
  try { ch.terminal.close(); } catch { /* already closed */ } // SIGHUP -> foreground group
  // Guard: kill(-1) / kill(-0) would signal every reachable process.
  if (leader > 1) try { process.kill(-leader, "SIGTERM"); } catch { /* gone / no group */ }
  setTimeout(() => {
    for (const pid of tree) {
      if (!isProcessAlive(pid)) continue;
      if (births) {
        const birthAtSnapshot = births.get(pid) ?? null;
        const birthNow = procStartTimeTicks(pid);
        // Skip only when /proc PROVES the live pid is not the one we snapshotted
        // (different start time, or vanished while still killable — i.e. now
        // someone else's). An unreadable snapshot keeps legacy behavior: an
        // unverifiable member must not weaken the every-survivor-dies guarantee.
        if (birthAtSnapshot !== null && birthNow !== birthAtSnapshot) continue;
      }
      try { process.kill(pid, "SIGKILL"); } catch { /* already reaped */ }
    }
  }, REAP_GRACE_MS);
}

/** Graceful reap of one channel's whole tree + escalation. */
export async function reapChannelTree(ch: Channel): Promise<void> {
  const platform = supportedHostPlatform();
  switch (platform) {
    case "darwin":
    case "linux":
      reapPosixChannel(ch);
      return;
    case "win32":
      if (!ch.jobHost) throw new Error(`Windows channel ${ch.childPid} has no job-host`);
      try {
        await ch.jobHost.close();
      } finally {
        // close() settles only after the helper has exited. A resolved close
        // additionally proves JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO.
        try { ch.terminal.close(); } catch { /* already closed */ }
      }
      return;
    default:
      return assertNeverPlatform(platform);
  }
}

function reapAllPosixChannelsSync(channels: Map<number, Channel>): void {
  const victims: number[] = [];
  for (const ch of channels.values()) {
    victims.push(...collectProcessTree(ch.proc.pid));
    try { ch.terminal.close(); } catch { /* ignore */ }
  }
  for (const pid of victims) if (isProcessAlive(pid)) {
    try { process.kill(pid, "SIGKILL"); } catch { /* ignore */ }
  }
}

/** Reap every channel before keeper shutdown. Windows must wait for each job. */
export async function reapAllChannels(channels: Map<number, Channel>): Promise<void> {
  const platform = supportedHostPlatform();
  switch (platform) {
    case "darwin":
    case "linux":
      reapAllPosixChannelsSync(channels);
      return;
    case "win32": {
      const results = await Promise.allSettled(
        Array.from(channels.values(), (channel) => reapChannelTree(channel)),
      );
      const rejected = results.find((result) => result.status === "rejected");
      if (rejected?.status === "rejected") throw rejected.reason;
      return;
    }
    default:
      return assertNeverPlatform(platform);
  }
}
