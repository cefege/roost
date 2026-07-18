// RC2 regression: the keeper retains a per-channel seqno-stamped output
// ring so a fresh worker can re-read head_seq + history on resume() instead
// of zeroing — history survives a worker restart (the keeper outlives the
// worker). Drives MultiplexedKeeperPool.getHistory directly against a real
// keeper subprocess + raw `cat` echo, so the keeper ring, the GetHistory/
// GetHistoryResp frames, and the client roundtrip are all exercised.
//
// Guards: project_scrollback_raw_ring_single_source (RC2). If a future
// keeper change drops the ring or breaks GetHistory, head_seq resets to 0
// on resume and the SPA's persisted lastSeq goes stale → lost history +
// seq-epoch reset. CLAUDE.md L11.

import { describe, test, expect, afterAll } from "bun:test";
import { existsSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { homedir, tmpdir } from "node:os";
import { MultiplexedKeeperPool, type MuxChannelCallbacks } from "../src/keeper/multiplexed-client.ts";

const SOCK_DIR = join(tmpdir(), `roost-test-keeper-hist-${process.pid}`);
process.env.ROOST_WORKER_DATA_DIR = SOCK_DIR;
process.env.ROOST_KEEPER_QUIET = "1";

const pool = new MultiplexedKeeperPool();
let _nextCh = 700;

afterAll(() => {
  pool.dispose();
  const sock = join(SOCK_DIR, "mux-keeper.sock");
  if (existsSync(sock)) try { unlinkSync(sock); } catch { /* ignore */ }
});

/** Spawn raw-mode cat on a fresh channel; returns helpers to send input
 *  and wait for the keeper to have observed N bytes of output. */
async function spawnRawCat(): Promise<{
  channelId: number;
  send: (s: string) => void;
  settle: () => Promise<void>;
}> {
  const channelId = _nextCh++;
  const cb: MuxChannelCallbacks = {
    onOutput: () => { /* the keeper ring is what we assert on, not the client capture */ },
    onExit: () => { /* don't care */ },
    onError: () => { /* don't care */ },
  };
  await pool.spawn({
    channelId, cwd: homedir(),
    argv: ["/bin/sh", "-c", "stty raw -echo 2>/dev/null; exec /bin/cat"],
    cols: 200, rows: 50,
    env: { TERM: "xterm-256color" },
    callbacks: cb,
  });
  await Bun.sleep(150);
  return {
    channelId,
    send: (s) => pool.input(channelId, new TextEncoder().encode(s)),
    settle: () => Bun.sleep(250),
  };
}

const dec = (u: Uint8Array) => new TextDecoder().decode(u);

describe("RC2 — keeper retains per-channel ring for cross-restart resume", () => {
  test("getHistory returns the echoed output; head_seq === ring length under cap", async () => {
    const ch = await spawnRawCat();
    const marker = "RC2-MARKER-7f3a9c-history-survives-restart";
    ch.send(marker);
    await ch.settle();

    const hist = await pool.getHistory(ch.channelId);
    // Under the 8 MB cap the ring holds the full lifetime, so head_seq
    // (total bytes ever output) equals the retained ring length.
    expect(hist.headSeq).toBe(hist.bytes.length);
    expect(hist.headSeq).toBeGreaterThan(0);
    // The raw-cat echo of our marker is in the retained ring.
    expect(dec(hist.bytes)).toContain(marker);
  });

  test("head_seq advances monotonically as more output accrues", async () => {
    const ch = await spawnRawCat();
    ch.send("first-chunk-RC2");
    await ch.settle();
    const a = await pool.getHistory(ch.channelId);
    ch.send("second-chunk-RC2");
    await ch.settle();
    const b = await pool.getHistory(ch.channelId);
    expect(b.headSeq).toBeGreaterThan(a.headSeq);
    expect(dec(b.bytes)).toContain("first-chunk-RC2");
    expect(dec(b.bytes)).toContain("second-chunk-RC2");
  });

  test("unknown channel resolves empty (resume falls back to fresh, not error)", async () => {
    const hist = await pool.getHistory(60000);
    expect(hist.headSeq).toBe(0);
    expect(hist.bytes.length).toBe(0);
  });
});
