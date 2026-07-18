// Guards the reverse-reap: SessionManager.reapStrayKeeperChannels() MUST kill
// any keeper PTY the worker no longer tracks (this.sessions is authoritative),
// so a deleted session's process can't outlive the row. Root cause it defends
// (memory project_close_pane_reap_process_tree / L11): kill() emits `closed`
// optimistically → coord DELETEs the row → but KillChild no-ops on a
// channel-mismatched keeper → zsh+claude survive with no owner and nothing ever
// sweeps them (12 coord rows vs 88 live PTYs).
//
// Real keeper subprocess + real PTYs (feedback_no_mock_claude_use_real). The
// pool singleton is isolated to a per-pid tmp sock via ROOST_WORKER_DATA_DIR set
// BEFORE any import touches muxSocketPath() — so this can never signal the
// daily-driver keeper (feedback_never_mass_kill_live_sessions).

import { join } from "node:path";
import { tmpdir, homedir } from "node:os";
import { existsSync, unlinkSync } from "node:fs";

const SOCK_DIR = join(tmpdir(), `roost-test-stray-reap-${process.pid}`);
process.env.ROOST_WORKER_DATA_DIR = SOCK_DIR;
process.env.ROOST_KEEPER_QUIET = "1";
const SOCK_PATH = join(SOCK_DIR, "mux-keeper.sock");

import { describe, test, expect, afterAll } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { getMultiplexedPool, type MuxChannelCallbacks } from "../src/keeper/multiplexed-client.ts";
import { asWorkerFp } from "@roost/shared";

const pool = getMultiplexedPool();
const TRACKED_CH = 900;
const STRAY_CH = 901;

const noopCallbacks: MuxChannelCallbacks = {
  onOutput: () => { /* lifecycle-only assertions */ },
  onExit: () => { /* expected when the stray is reaped */ },
  onError: () => { /* keeper death path */ },
};

function freshMgr(): SessionManager {
  return new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
    hookSocketPath: "/dev/null",
  });
}

async function spawnRawCat(channelId: number): Promise<void> {
  await pool.spawn({
    channelId, cwd: homedir(),
    argv: ["/bin/sh", "-c", "stty raw -echo 2>/dev/null; exec /bin/cat"],
    cols: 200, rows: 50,
    env: { TERM: "xterm-256color" },
    callbacks: noopCallbacks,
  });
}

async function liveChannelIds(): Promise<number[]> {
  return (await pool.listChannelsFresh()).map(c => c.channelId);
}

async function waitFor(predicate: () => Promise<boolean> | boolean, deadlineMs: number, stepMs = 40): Promise<boolean> {
  const deadline = Date.now() + deadlineMs;
  while (Date.now() < deadline) {
    if (await predicate()) return true;
    await Bun.sleep(stepMs);
  }
  return Boolean(await predicate());
}

function killKeeperBySock(): void {
  const out = Bun.spawnSync(["pgrep", "-f", SOCK_PATH]).stdout.toString().trim();
  for (const line of out.split("\n")) {
    const pid = Number(line.trim());
    if (Number.isInteger(pid) && pid > 0 && pid !== process.pid) {
      try { process.kill(pid, "SIGKILL"); } catch { /* already dead */ }
    }
  }
}

afterAll(() => {
  pool.dispose();
  killKeeperBySock();
  if (existsSync(SOCK_PATH)) try { unlinkSync(SOCK_PATH); } catch { /* ignore */ }
});

describe("keeper-stray-reap — reverse-reap kills untracked keeper PTYs, two-strike", () => {
  test("stray channel dies after 2 sweeps; tracked channel never dies; one sweep is a no-op", async () => {
    const mgr = freshMgr();
    // Silence the manager's unrelated background sweeps (detect/viewport/auto-
    // stray) so this test drives reapStrayKeeperChannels() by hand and the
    // detect sweep doesn't trip over the minimal injected record.
    const timers = mgr as unknown as Record<string, ReturnType<typeof setInterval>>;
    clearInterval(timers.detectSweepTimer);
    clearInterval(timers.viewportReaperTimer);
    clearInterval(timers.strayReaperTimer);

    // Two real PTYs on the isolated keeper. Only TRACKED_CH is registered in
    // this.sessions — STRAY_CH is a ghost (as if its session row was deleted
    // but KillChild no-op'd).
    await spawnRawCat(TRACKED_CH);
    await spawnRawCat(STRAY_CH);
    (mgr as unknown as { sessions: Map<number, unknown> }).sessions.set(TRACKED_CH, { channelId: TRACKED_CH });

    expect(await liveChannelIds()).toEqual(expect.arrayContaining([TRACKED_CH, STRAY_CH]));

    // Strike 1: records the stray, kills nothing.
    const firstPass = await mgr.reapStrayKeeperChannels();
    expect(firstPass).toBe(0);
    await Bun.sleep(200);
    expect(await liveChannelIds()).toEqual(expect.arrayContaining([TRACKED_CH, STRAY_CH]));

    // Strike 2: reaps the stray.
    const secondPass = await mgr.reapStrayKeeperChannels();
    expect(secondPass).toBe(1);

    // Stray gone; tracked survives.
    const strayGone = await waitFor(async () => !(await liveChannelIds()).includes(STRAY_CH), 6000);
    expect(strayGone).toBe(true);
    expect(await liveChannelIds()).toContain(TRACKED_CH);

    // A subsequent sweep is a clean no-op — the tracked channel is never a stray.
    const thirdPass = await mgr.reapStrayKeeperChannels();
    expect(thirdPass).toBe(0);
    expect(await liveChannelIds()).toContain(TRACKED_CH);

    pool.kill(TRACKED_CH);
  }, 30_000);
});
