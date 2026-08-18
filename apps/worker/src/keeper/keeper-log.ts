// Keeper logging + FD diagnostics. Split out of multiplexed-main.ts so the
// keeper entry, frame handler, and future keeper modules share one logger
// without re-declaring it.

import * as fs from "node:fs";

// Tiny structured logger so keeper stderr is grep-able JSON instead of
// plain text. Same shape as `@roost/shared/log` (target, msg, fields)
// but kept inline because the keeper is its own subprocess and we
// don't want to pull the shared package down through its own module
// resolution chain — keeper has to boot fast and stay portable across
// the bun-install layouts on different Macs.
//
// stderr is wired to ~/Library/Logs/RoostWorker/keeper.err.log by
// multiplexed-client.ts:107.
export function _log(level: "info" | "warn" | "error", target: string, msg: string, fields: Record<string, unknown> = {}): void {
  const line = JSON.stringify({ ts: Date.now(), level, target, msg, ...fields });
  process.stderr.write(line + "\n");
}

// Count this keeper process's open file descriptors. macOS exposes one
// /dev/fd/<n> entry per open fd of the CALLING process, so readdir length
// is the live fd count. Attached to dead-birth / spawn_failed logs ONLY
// (never the hot path) to settle the unproven "degraded keeper = FD/pty
// exhaustion" question (docs/FAILURE-INDEX.md / project_keeper_degradation_dead_birth_selfheal):
// if open_fds climbs toward the soft limit (256 on a default LaunchAgent)
// across dead-births, exhaustion is the mechanism. -1 = couldn't read.
export function _keeperOpenFdCount(): number {
  try { return fs.readdirSync("/dev/fd").length; } catch { return -1; }
}
