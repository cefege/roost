// Detect the TCP ports a session's process tree is LISTENing on, so the
// sidebar can chip a dev server (vite :5174) and click through to it. Only the
// worker can see the host's sockets; pushed to coord/SPA via the `ports`
// SessionEvent → chips in apps/web/src/components/sidebar/FolderList.tsx.
// Called by session-manager.ts (_startPorts) on spawn + a 90s poll.
//
// POSIX tools run under service managers with minimal PATHs. Windows never
// invokes ps/ss/lsof; process and socket ownership comes from the typed helper.
import { log } from "@roost/shared";
import {
  assertNeverPlatform,
  supportedHostPlatform,
} from "@roost/shared/platform";
import {
  windowsListeningPorts,
  windowsProcessSnapshot,
  type WindowsListeningPort,
} from "@roost/shared/windows-helper";

const HOST_PLATFORM = supportedHostPlatform();
let TOOL_PATH: string;
switch (HOST_PLATFORM) {
  case "linux":
    TOOL_PATH = `/usr/local/bin:/usr/sbin:/usr/bin:/bin:${process.env.PATH ?? ""}`;
    break;
  case "darwin":
    TOOL_PATH = `/opt/homebrew/bin:/usr/local/bin:/usr/sbin:/usr/bin:/bin:${process.env.PATH ?? ""}`;
    break;
  case "win32":
    TOOL_PATH = process.env.PATH ?? process.env.Path ?? "";
    break;
  default:
    assertNeverPlatform(HOST_PLATFORM);
}

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

/** All descendant pids of `root` (inclusive), via one platform snapshot. */
async function descendantPids(root: number): Promise<number[]> {
  const children = new Map<number, number[]>();
  if (HOST_PLATFORM === "win32") {
    for (const record of await windowsProcessSnapshot()) {
      const list = children.get(record.ppid);
      if (list) list.push(record.pid);
      else children.set(record.ppid, [record.pid]);
    }
  } else {
    const out = await run(["ps", "-Ao", "pid,ppid"]);
    if (!out) return [root];
    for (const line of out.split("\n").slice(1)) {
      const m = line.trim().match(/^(\d+)\s+(\d+)/);
      if (!m) continue;
      const pid = Number(m[1]);
      const ppid = Number(m[2]);
      const list = children.get(ppid);
      if (list) list.push(pid);
      else children.set(ppid, [pid]);
    }
  }
  const seen = new Set<number>([root]);
  const stack = [root];
  while (stack.length) {
    const pid = stack.pop()!;
    for (const child of children.get(pid) ?? []) {
      if (!seen.has(child)) {
        seen.add(child);
        stack.push(child);
      }
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

/** Same contract as parseReachableListenPorts, for `ss -ltnpH` rows:
 *  `LISTEN 0 511 0.0.0.0:5173 0.0.0.0:* users:(("bun",pid=1234,fd=20))`
 *  ss has no `-p <pids>` filter, so the pid set is applied here. */
export function parseSsListenPorts(out: string, pids: Set<number>): number[] {
  const ports = new Set<number>();
  for (const line of out.split("\n")) {
    const fields = line.trim().split(/\s+/);
    const local = fields[3];
    if (!local) continue;
    const cut = local.lastIndexOf(":");
    if (cut <= 0) continue;
    const host = local.slice(0, cut);
    const port = Number(local.slice(cut + 1));
    if (!Number.isInteger(port) || port <= 0) continue;
    // Same loopback rule as the lsof parser: a 127.x/::1 bind never answers
    // on the worker's tailnet IP, so its chip would be a dead link.
    if (host === "[::1]" || host === "::1" || /^127\./.test(host)) continue;
    // Keep the row only if it belongs to this session's process tree.
    let owned = false;
    for (const m of line.matchAll(/\bpid=(\d+)/g)) {
      if (pids.has(Number(m[1]))) { owned = true; break; }
    }
    if (!owned) continue;
    ports.add(port);
  }
  return [...ports].sort((a, b) => a - b);
}

/** Filter structured GetExtendedTcpTable rows to a session process tree. */
export function filterWindowsListenPorts(
  records: readonly WindowsListeningPort[],
  pids: ReadonlySet<number>,
): number[] {
  const ports = new Set<number>();
  for (const record of records) {
    if (!pids.has(record.pid)) continue;
    const address = record.address.replace(/^\[(.*)\]$/, "$1");
    if (address === "::1" || /^127\./.test(address)) continue;
    ports.add(record.port);
  }
  return [...ports].sort((a, b) => a - b);
}

/** Distinct reachable LISTEN ports held by `root`'s process tree, ascending. */
export async function readListeningPorts(root: number | null | undefined): Promise<number[]> {
  if (!root || root <= 0) return [];
  try {
    const pids = await descendantPids(root);
    if (pids.length === 0) return [];
    switch (HOST_PLATFORM) {
      case "linux": {
        // ss has no pid selector; list every listening socket with its owning
        // process and filter to the tree here.
        const out = await run(["ss", "-ltnpH"]);
        if (!out) return [];
        return parseSsListenPorts(out, new Set(pids));
      }
      case "darwin": {
        // -a ANDs the pid filter with LISTEN. Without it lsof ORs selection
        // types and leaks every listening socket on the host.
        const out = await run([
          "lsof", "-nP", "-iTCP", "-sTCP:LISTEN", "-a", "-p", pids.join(","),
        ]);
        if (!out) return [];
        return parseReachableListenPorts(out);
      }
      case "win32":
        return filterWindowsListenPorts(await windowsListeningPorts(), new Set(pids));
      default:
        return assertNeverPlatform(HOST_PLATFORM);
    }
  } catch (error) {
    log.warn("listening-ports", "scan_failed", {
      platform: HOST_PLATFORM,
      root_pid: root,
      error: String(error),
    });
    return [];
  }
}

/** Order-insensitive equality so the poll only emits on a real change. */
export function portsEq(a: number[] | undefined, b: number[] | undefined): boolean {
  const x = a ?? [], y = b ?? [];
  if (x.length !== y.length) return false;
  for (let i = 0; i < x.length; i++) if (x[i] !== y[i]) return false;
  return true;
}
