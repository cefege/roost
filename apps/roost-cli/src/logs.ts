// `roost logs <app> [--tail N]` — tail an app's log files.
// app ∈ {coord, worker}. --tail N passes -n N to tail (default: last 100 lines).
// Emits a warning if any log file exceeds 100 MB.

import { spawn } from "bun";
import { existsSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { workerLogDir } from "@roost/shared/paths";

const LOG_ROTATE_WARN_BYTES = 100 * 1024 * 1024; // 100 MB

// The coordinator is macOS-only, but `roost logs coord` on Linux should
// point at the same XDG state dir the worker uses rather than a literal
// ~/Library path that can never exist there.
const coordLogDir = process.platform === "darwin"
  ? join(homedir(), "Library", "Logs", "RoostCoord")
  : join(process.env.XDG_STATE_HOME ?? join(homedir(), ".local", "state"), "RoostCoord");

const PATHS: Record<string, string[]> = {
  coord: [join(coordLogDir, "main.out.log"), join(coordLogDir, "main.err.log")],
  worker: [join(workerLogDir(), "main.out.log"), join(workerLogDir(), "main.err.log")],
};

function parseArgs(args: string[]): { app: string | undefined; tailLines: string } {
  let app: string | undefined;
  let tailLines = "100";
  for (let i = 0; i < args.length; i++) {
    const a = args[i]!;
    if (a === "--tail" || a === "-n") {
      const n = args[i + 1];
      if (n && /^\d+$/.test(n)) {
        tailLines = n;
        i++;
      }
    } else if (!app) {
      app = a;
    }
  }
  return { app, tailLines };
}

function warnLargeFiles(paths: string[]): void {
  for (const p of paths) {
    if (!existsSync(p)) continue;
    const sz = statSync(p).size;
    if (sz > LOG_ROTATE_WARN_BYTES) {
      const mb = (sz / (1024 * 1024)).toFixed(0);
      // newsyslog is macOS-only; on Linux the truncate is the whole remedy.
      const rotate = process.platform === "darwin"
        ? ` Rotate via: sudo newsyslog -vf /etc/newsyslog.d/roost-coord.conf or`
        : "";
      console.warn(
        `[roost-logs] ${p} is ${mb}MB (>${LOG_ROTATE_WARN_BYTES / 1024 / 1024}MB).` +
        `${rotate} truncate: > "${p}"`,
      );
    }
  }
}

export async function logs(args: string[]): Promise<void> {
  const { app, tailLines } = parseArgs(args);
  if (!app || !(app in PATHS)) {
    console.error("usage: roost logs <coord|worker> [--tail N]");
    process.exit(1);
  }
  const paths = PATHS[app]!;
  warnLargeFiles(paths);
  const proc = spawn({
    cmd: ["tail", "-F", "-n", tailLines, ...paths],
    stdio: ["inherit", "inherit", "inherit"],
  });
  await proc.exited;
}
