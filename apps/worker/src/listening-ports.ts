// Detect the TCP ports a session's process tree is LISTENing on, so the
// sidebar can chip a dev server (vite :5174) and click through to it. Only the
// worker can see the host's sockets; pushed to coord/SPA via the `ports`
// SessionEvent → chips in apps/web/src/components/sidebar/FolderList.tsx.
// Called by session-manager.ts (_startPorts) on spawn + a 90s poll.
//
// macOS/BSD tools (ps + lsof). Every failure path yields [] — never throws.
// Mirrors pr-status.ts.

// The worker runs under a LaunchAgent whose PATH is minimal (/usr/bin:/bin) —
// lsof lives in /usr/sbin, absent from it, so a bare `lsof` spawn ENOENTs and
// the port scan silently returns []. Augment PATH for every tool spawn.
// (CLAUDE.md L11 LaunchAgent-env class, same as the TERM=unknown row.)
const TOOL_PATH = `/opt/homebrew/bin:/usr/local/bin:/usr/sbin:/usr/bin:/bin:${process.env.PATH ?? ""}`;

async function run(cmd: string[]): Promise<string | null> {
  try {
    const proc = Bun.spawn(cmd, { stdout: "pipe", stderr: "ignore", env: { ...process.env, PATH: TOOL_PATH } });
    const out = await new Response(proc.stdout).text();
    await proc.exited;
    return proc.exitCode === 0 ? out : null;
  } catch {
    return null;
  }
}

/** All descendant pids of `root` (inclusive), via one `ps -eo pid,ppid` walk. */
async function descendantPids(root: number): Promise<number[]> {
  const out = await run(["ps", "-Ao", "pid,ppid"]);
  if (!out) return [root];
  const children = new Map<number, number[]>();
  for (const line of out.split("\n").slice(1)) {
    const m = line.trim().match(/^(\d+)\s+(\d+)/);
    if (!m) continue;
    const pid = Number(m[1]);
    const ppid = Number(m[2]);
    (children.get(ppid) ?? children.set(ppid, []).get(ppid)!).push(pid);
  }
  const seen = new Set<number>([root]);
  const stack = [root];
  while (stack.length) {
    const p = stack.pop()!;
    for (const c of children.get(p) ?? []) {
      if (!seen.has(c)) { seen.add(c); stack.push(c); }
    }
  }
  return [...seen];
}

/** Reachable LISTEN ports parsed from `lsof -nP -iTCP -sTCP:LISTEN` output —
 *  ascending, distinct. Pure (DOM/proc-free) so it's unit-tested directly.
 *
 *  ONLY ports with a bind on a non-loopback interface (`*`, `0.0.0.0`, `[::]`,
 *  or a concrete LAN/tailnet IP) are kept: the folder chip opens them at
 *  `http://<worker reachable_addr>:<port>`, and a loopback-only server (the
 *  vite/next default, language servers, debuggers) does NOT answer on the
 *  worker's tailnet IP — its chip was always a dead link. Dropping those is why
 *  a localhost-bound dev server now shows no chip: it isn't reachable from
 *  another device at all (run it with `--host` to expose it). */
export function parseReachableListenPorts(lsofOutput: string): number[] {
  const ports = new Set<number>();
  for (const line of lsofOutput.split("\n")) {
    // NAME's last token before "(LISTEN)" is HOST:PORT — e.g. `*:5173`,
    // `0.0.0.0:5173`, `127.0.0.1:5173`, `[::1]:5173`, `[::]:5173`, `100.x:5173`.
    // Keep only non-loopback binds: a `127.x`/`[::1]` server never answers on
    // the worker's tailnet IP, so its `http://<reachable_addr>:port` chip is a
    // guaranteed dead link. lsof runs numeric (-n) so hosts are never DNS names.
    const m = line.match(/\s(\S+):(\d+)\s*\(LISTEN\)\s*$/);
    const host = m?.[1];
    if (!host || host === "[::1]" || host === "::1" || /^127\./.test(host)) continue;
    ports.add(Number(m![2]));
  }
  return [...ports].sort((a, b) => a - b);
}

/** Distinct reachable LISTEN ports held by `root`'s process tree, ascending.
 *  [] on any failure / none. `lsof -p` takes a comma-list. */
export async function readListeningPorts(root: number | null | undefined): Promise<number[]> {
  if (!root || root <= 0) return [];
  const pids = await descendantPids(root);
  if (pids.length === 0) return [];
  // -a ANDs the pid filter with the LISTEN filter. WITHOUT it lsof ORs
  // different selection types → returns EVERY listening socket on the host,
  // not just this session's tree (verified: leaked redis/chrome/system ports).
  const out = await run(["lsof", "-nP", "-iTCP", "-sTCP:LISTEN", "-a", "-p", pids.join(",")]);
  if (!out) return [];
  return parseReachableListenPorts(out);
}

/** Order-insensitive equality so the poll only emits on a real change. */
export function portsEq(a: number[] | undefined, b: number[] | undefined): boolean {
  const x = a ?? [], y = b ?? [];
  if (x.length !== y.length) return false;
  for (let i = 0; i < x.length; i++) if (x[i] !== y[i]) return false;
  return true;
}
