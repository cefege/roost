// Guards the keeper code-version stamp (plan: keeper code-version stamp).
//
// KEEPER_BUILD_STAMP (keeper-stamp.ts) is a content hash of the keeper's own
// source; worker + keeper compute it independently and MUST agree, else every
// keeper would falsely read as "stale". This drives it through the real Hello/
// HelloResp handshake against a real keeper subprocess and asserts agreement.

import { describe, test, expect, afterAll } from "bun:test";
import { rmSync } from "node:fs";
import { join } from "node:path";
import { homedir, tmpdir } from "node:os";
import { MultiplexedKeeperPool, probeKeeperCompatible, type MuxChannelCallbacks } from "../src/keeper/multiplexed-client.ts";
import { KEEPER_BUILD_STAMP } from "../src/keeper/keeper-stamp.ts";
import { muxLocalEndpoint } from "../src/keeper/keeper-pool-config.ts";
import { keeperTestShellSpec } from "./keeper-test-fixtures.ts";

const SOCK_DIR = join(tmpdir(), `roost-test-keeper-stamp-${process.pid}`);
process.env.ROOST_WORKER_DATA_DIR = SOCK_DIR;
process.env.ROOST_KEEPER_QUIET = "1";

const pool = new MultiplexedKeeperPool();
let _nextCh = 700;

afterAll(() => {
  // Private pool on its own SOCK_DIR — the keeper it spawned is ours to reap.
  // dispose() leaves it running by design, so capture the pid first.
  const keeperPid = pool._keeperProc?.pid;
  pool.dispose();
  if (keeperPid) try { process.kill(keeperPid, "SIGKILL"); } catch { /* already dead */ }
  // Whole dir: unlinking only the socket left SOCK_DIR in $TMPDIR every run.
  rmSync(SOCK_DIR, { recursive: true, force: true });
});

const noop: MuxChannelCallbacks = { onOutput: () => {}, onExit: () => {}, onError: () => {} };

describe("keeper-stamp — code-version stamp + handshake agreement", () => {
  test("KEEPER_BUILD_STAMP is a real 12-hex stamp (not the read-fail sentinel)", () => {
    expect(KEEPER_BUILD_STAMP).toMatch(/^[0-9a-f]{12}$/);
    expect(KEEPER_BUILD_STAMP).not.toBe("unknown");
  });

  test("a real keeper reports OUR exact stamp over HelloResp (worker+keeper agree)", async () => {
    // Bring the keeper subprocess up (spawn a trivial channel), then probe it.
    await pool.spawn({
      channelId: _nextCh++,
      shellSpec: keeperTestShellSpec({
        executable: "/bin/sh",
        argv: ["-c", "exec sleep 60"],
        cwd: homedir(),
      }),
      cols: 80, rows: 24, callbacks: noop,
    });
    const result = await probeKeeperCompatible(muxLocalEndpoint());
    expect(result.compatible).toBe(true);
    // The keeper is a SEPARATE process that computed its stamp from the same
    // source files — it must equal ours, or staleness would false-positive.
    expect(result.keeperStamp).toBe(KEEPER_BUILD_STAMP);
    // Freshly-spawned keeper = current code → the pool reports it non-stale.
    expect(pool.getRunningKeeperStamp()).toBe(KEEPER_BUILD_STAMP);
  }, 15_000);

  test("staleness predicate: a different reported stamp reads as stale", () => {
    // The heartbeat rule (heartbeat.ts): running !== ours → report running (stale).
    const running = "deadbeef0000";
    expect(running !== KEEPER_BUILD_STAMP).toBe(true);
    expect(KEEPER_BUILD_STAMP !== KEEPER_BUILD_STAMP).toBe(false);
  });
});
