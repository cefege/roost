// Process-tree reaping for the multiplexed keeper. Split out of
// multiplexed-main.ts; the entry owns the single `channels` Map and passes it
// into reapAllChannelsSync so there is exactly one source of truth.

import type { Channel } from "./keeper-types.ts";

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

/** kill(pid,0) liveness probe; ESRCH => gone. Guards self + pid<=1. */
function isProcessAlive(pid: number): boolean {
  if (pid <= 1 || pid === process.pid) return false;
  try { process.kill(pid, 0); return true; } catch { return false; }
}

/** Graceful reap of one channel's whole tree + escalation. */
export function reapChannelTree(ch: Channel): void {
  const leader = ch.proc.pid;
  const tree = collectProcessTree(leader);
  try { ch.proc.terminal?.close(); } catch { /* already closed */ }   // SIGHUP -> fg group + reclaims master fd
  // Guard: kill(-1) / kill(-0) would signal EVERY reachable process. `leader`
  // is always a real spawned pid (>1), but never let that invariant slip.
  if (leader > 1) try { process.kill(-leader, "SIGTERM"); } catch { /* gone / no group */ }
  setTimeout(() => {
    for (const pid of tree) if (isProcessAlive(pid)) {
      try { process.kill(pid, "SIGKILL"); } catch { /* already reaped */ }
    }
  }, REAP_GRACE_MS);
}

/** Synchronous all-channel sweep for keeper shutdown (no grace window — the
 *  host is exiting; a deferred timer would never fire). SIGKILL every child so
 *  none orphan to launchd on deploy/kickstart. */
export function reapAllChannelsSync(channels: Map<number, Channel>): void {
  const victims: number[] = [];
  for (const ch of channels.values()) {
    victims.push(...collectProcessTree(ch.proc.pid));
    try { ch.proc.terminal?.close(); } catch { /* ignore */ }
  }
  for (const pid of victims) if (isProcessAlive(pid)) {
    try { process.kill(pid, "SIGKILL"); } catch { /* ignore */ }
  }
}
