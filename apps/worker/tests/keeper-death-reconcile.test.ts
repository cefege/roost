// Guards commit 5192d2c5 + 5bdc2ab4 (memory project_keeper_death_auto_respawn):
// MultiplexedKeeperPool.setOnKeeperDeath(fn) MUST fire fn() once when the
// worker→keeper UDS closes (= keeper subprocess died mid-life), and the pool
// MUST recover — a subsequent pool.spawn() re-ensure()s a FRESH keeper. Without
// the callback the boot-only reconcile never re-ran → every PTY orphaned,
// terminals "not connected", input dropped. CLAUDE.md keeper-death row / L11.
//
// Real keeper subprocess (no mocks, per feedback_no_mock_claude_use_real).
// Death trigger = kill -9 of the keeper pid (found via pgrep -f on the unique
// per-pid tmp sock path). Chosen over socket-unlink self-suicide because the
// keeper's unlink poll runs every 30s (multiplexed-main.ts:332) — too slow to
// be non-flaky; SIGKILL closes the UDS immediately → deterministic s.on("close").

import { describe, test, expect, afterAll } from "bun:test";
import { rmSync } from "node:fs";
import { join } from "node:path";
import { homedir, tmpdir } from "node:os";
import { MultiplexedKeeperPool, type MuxChannelCallbacks } from "../src/keeper/multiplexed-client.ts";

const SOCK_DIR = join(tmpdir(), `roost-test-keeper-death-${process.pid}`);
process.env.ROOST_WORKER_DATA_DIR = SOCK_DIR;
process.env.ROOST_KEEPER_QUIET = "1";
const SOCK_PATH = join(SOCK_DIR, "mux-keeper.sock");

const pool = new MultiplexedKeeperPool();
let _nextCh = 800;

afterAll(() => {
  // dispose() leaves the keeper running by design; capture the spawned pid
  // first and kill it directly, with the sock-path pgrep as fallback for a
  // keeper respawned after a kill-9 test.
  const keeperPid = pool._keeperProc?.pid;
  pool.dispose();
  if (keeperPid) try { process.kill(keeperPid, "SIGKILL"); } catch { /* already dead */ }
  killKeeperBySock(); // reap any keeper left running after a kill-9 test
  // Whole dir: unlinking only the socket left SOCK_DIR in $TMPDIR every run.
  rmSync(SOCK_DIR, { recursive: true, force: true });
});

const noopCallbacks: MuxChannelCallbacks = {
  onOutput: () => { /* the pool lifecycle is what we assert on */ },
  onExit: () => { /* expected on keeper death — close handler fires onExit(null) */ },
  onError: () => { /* expected on keeper death */ },
};

async function spawnRawCat(channelId: number): Promise<void> {
  await pool.spawn({
    channelId, cwd: homedir(),
    argv: ["/bin/sh", "-c", "stty raw -echo 2>/dev/null; exec /bin/cat"],
    cols: 200, rows: 50,
    env: { TERM: "xterm-256color" },
    callbacks: noopCallbacks,
  });
}

/** pgrep -f the keeper by its unique sock-path arg → pid (or null). The pool
 *  spawns `bun run .../multiplexed-main.ts <SOCK_PATH>`; SOCK_PATH carries
 *  process.pid so it's unique to this test run — no risk of matching a
 *  daily-driver keeper. */
function findKeeperPid(): number | null {
  const out = Bun.spawnSync(["pgrep", "-f", SOCK_PATH]).stdout.toString().trim();
  if (!out) return null;
  // pgrep can also match the pgrep invocation's own argv on some shells; take
  // the first numeric line that is NOT this test process.
  for (const line of out.split("\n")) {
    const pid = Number(line.trim());
    if (Number.isInteger(pid) && pid > 0 && pid !== process.pid) return pid;
  }
  return null;
}

function killKeeperBySock(): void {
  const pid = findKeeperPid();
  if (pid !== null) try { process.kill(pid, "SIGKILL"); } catch { /* already dead */ }
}

/** Poll `predicate` every `stepMs` until true or `deadlineMs` elapses. */
async function waitFor(predicate: () => boolean, deadlineMs: number, stepMs = 25): Promise<boolean> {
  const deadline = Date.now() + deadlineMs;
  while (Date.now() < deadline) {
    if (predicate()) return true;
    await Bun.sleep(stepMs);
  }
  return predicate();
}

describe("keeper-death-reconcile — setOnKeeperDeath fires + pool recovers", () => {
  test("callback fires within timeout when the keeper is SIGKILL'd; pool re-ensures a fresh keeper", async () => {
    let deathCount = 0;
    pool.setOnKeeperDeath(() => { deathCount++; });

    // Two channels on the SAME keeper so we also prove the callback is
    // per-death (socket-close), not per-channel.
    await spawnRawCat(_nextCh++);
    await spawnRawCat(_nextCh++);
    // Keeper is up + socket connected (spawn awaited ensure()).
    const pidBefore = await waitFor(() => findKeeperPid() !== null, 5000);
    expect(pidBefore).toBe(true);
    const keeperPid = findKeeperPid()!;
    expect(keeperPid).toBeGreaterThan(0);

    // Death trigger: SIGKILL the keeper subprocess → UDS closes →
    // s.on("close") → setTimeout(0, _onKeeperDeath).
    process.kill(keeperPid, "SIGKILL");

    const fired = await waitFor(() => deathCount >= 1, 8000);
    expect(fired).toBe(true);
    // Exactly once per death even with 2 channels open (close handler, not
    // per-channel onExit). Allow the poll a moment to catch a spurious second.
    await Bun.sleep(300);
    expect(deathCount).toBe(1);

    // Old keeper pid is gone (killed).
    const oldGone = await waitFor(() => {
      const p = findKeeperPid();
      return p === null || p !== keeperPid;
    }, 8000);
    expect(oldGone).toBe(true);

    // RECOVERY: a subsequent spawn must succeed — pool.ensure() re-dials and,
    // finding no live socket, spawns a FRESH keeper. Proves death is
    // recoverable, not permanent breakage.
    const recoveryChannel = _nextCh++;
    await spawnRawCat(recoveryChannel); // resolves only on SpawnAck from new keeper
    const freshKeeper = findKeeperPid();
    expect(freshKeeper).not.toBeNull();
    expect(freshKeeper).not.toBe(keeperPid);
  }, 30_000);

  test("a second SIGKILL fires the callback again (re-armed across deaths)", async () => {
    let deathCount = 0;
    pool.setOnKeeperDeath(() => { deathCount++; });

    await spawnRawCat(_nextCh++);
    const up = await waitFor(() => findKeeperPid() !== null, 5000);
    expect(up).toBe(true);
    const keeperPid = findKeeperPid()!;

    process.kill(keeperPid, "SIGKILL");
    const fired = await waitFor(() => deathCount >= 1, 8000);
    expect(fired).toBe(true);
    expect(deathCount).toBe(1);
  }, 20_000);
});
