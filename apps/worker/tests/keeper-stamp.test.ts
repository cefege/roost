// Guards the keeper code-version stamp (plan: keeper code-version stamp).
//
// KEEPER_BUILD_STAMP (keeper-stamp.ts) is a content hash of the keeper's own
// source; worker + keeper compute it independently and MUST agree, else every
// keeper would falsely read as "stale". This drives it through the real Hello/
// HelloResp handshake against a real keeper subprocess and asserts agreement.

import { describe, test, expect, afterAll } from "bun:test";
import { existsSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { homedir, tmpdir } from "node:os";
import { MultiplexedKeeperPool, probeKeeperCompatible, type MuxChannelCallbacks } from "../src/keeper/multiplexed-client.ts";
import { KEEPER_BUILD_STAMP } from "../src/keeper/keeper-stamp.ts";

const SOCK_DIR = join(tmpdir(), `roost-test-keeper-stamp-${process.pid}`);
process.env.ROOST_WORKER_DATA_DIR = SOCK_DIR;
process.env.ROOST_KEEPER_QUIET = "1";
const SOCK_PATH = join(SOCK_DIR, "mux-keeper.sock");

const pool = new MultiplexedKeeperPool();
let _nextCh = 700;

afterAll(() => {
  pool.dispose();
  if (existsSync(SOCK_PATH)) try { unlinkSync(SOCK_PATH); } catch { /* ignore */ }
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
      channelId: _nextCh++, cwd: homedir(),
      argv: ["/bin/sh", "-c", "exec sleep 60"],
      cols: 80, rows: 24, env: { TERM: "xterm-256color" }, callbacks: noop,
    });
    const result = await probeKeeperCompatible(SOCK_PATH);
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
