// `roost logs <app> [--tail N]` — tail an app's log files.
// app ∈ {coord, worker}. --tail N passes -n N to tail (default: last 100 lines).
// Emits a warning if any log file exceeds 100 MB.

import { spawn } from "bun";
import { existsSync, statSync } from "node:fs";

const LOG_ROTATE_WARN_BYTES = 100 * 1024 * 1024; // 100 MB

const PATHS: Record<string, string[]> = {
  coord: [
    `${process.env.HOME}/Library/Logs/RoostCoord/main.out.log`,
    `${process.env.HOME}/Library/Logs/RoostCoord/main.err.log`,
  ],
  worker: [
    `${process.env.HOME}/Library/Logs/RoostWorker/main.out.log`,
    `${process.env.HOME}/Library/Logs/RoostWorker/main.err.log`,
  ],
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
      console.warn(
        `[roost-logs] ${p} is ${mb}MB (>${LOG_ROTATE_WARN_BYTES / 1024 / 1024}MB).` +
        ` Rotate via: sudo newsyslog -vf /etc/newsyslog.d/roost-coord.conf` +
        ` or truncate: > "${p}"`,
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
