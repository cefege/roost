// Suite-wide safety net for the real-keeper tests. Registered via
// apps/worker/bunfig.toml [test] preload, so this afterAll runs ONCE after
// every test file — the only in-process hook late enough to see the leak.
//
// The leak it closes: ROOST_WORKER_DATA_DIR is a process-GLOBAL env var, but
// `bun test` loads all files into ONE process. The keeper test files pin it at
// module scope to their own `roost-test-<label>-<pid>` dir; the ~9 other files
// that build a SessionManager never pin anything, and SessionManager's
// constructor calls pool.ensure() (session-manager.ts:177). So a file like
// wterm-rebuild-determinism.test.ts spawns a keeper into whichever keeper
// file's dir happens to be current — long after that file's afterAll reaped
// and removed it. No per-file teardown can see that; a global one can.
//
// `process.on("exit")` is NOT usable here: bun does not fire it under
// `bun test` (verified). Hence the preload afterAll.
//
// Scoped to `-${process.pid}`: every fixture dir ends in the test-runner pid,
// so a concurrent `bun test` elsewhere is untouched, and the daily-driver
// keeper under ~/Library can never match (feedback_never_mass_kill_live_sessions).
//
// Census uses `ps`, NOT `pgrep -f`: pgrep silently fails to match argv
// containing a space, which the production keeper's sock path has — it cannot
// be trusted to enumerate keepers.

import { afterAll } from "bun:test";
import { readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const MARKER = `-${process.pid}`;

afterAll(() => {
  // 1. Reap keepers whose socket lives in one of THIS run's fixture dirs.
  try {
    const out = Bun.spawnSync(["ps", "-Ao", "pid=,args="]).stdout.toString();
    for (const line of out.split("\n")) {
      const m = /^\s*(\d+)\s+(.*)$/.exec(line);
      if (!m) continue;
      const args = m[2];
      if (!args.includes("multiplexed-main.ts")) continue;
      if (!args.includes("roost-test-")) continue;
      if (!args.includes(`${MARKER}/mux-keeper.sock`)) continue;
      try { process.kill(Number(m[1]), "SIGKILL"); } catch { /* already dead */ }
    }
  } catch { /* ps unavailable — dir sweep below still runs */ }

  // 2. Remove this run's fixture dirs. Must follow the kill: a live keeper
  //    recreates its dir, which is how they survived per-file rmSync.
  try {
    const tmp = tmpdir();
    for (const name of readdirSync(tmp)) {
      if (!name.startsWith("roost-test-") || !name.endsWith(MARKER)) continue;
      try { rmSync(join(tmp, name), { recursive: true, force: true }); } catch { /* ignore */ }
    }
  } catch { /* ignore */ }
});
