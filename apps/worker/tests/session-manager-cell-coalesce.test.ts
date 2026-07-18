// Phase-3 (SSP rate governor): per-channel cell-delta coalescing. A burst
// of PtyOut chunks must emit ONE delta to the LATEST grid per coalesce window,
// not one frame per chunk — the wtermCore already holds every chunk's bytes, so
// the single coalesced read is "target = latest". Guards against reverting to a
// synchronous per-chunk emit (which floods the wire on resize storms / claude
// full-screen redraws).

import { describe, test, expect } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared";
import { initCellEmitState } from "@roost/shared/cell";
import { WasmBridge } from "@wterm/core";

function mgrWithCellCounter(): { mgr: SessionManager; frames: unknown[] } {
  const frames: unknown[] = [];
  const mgr = new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
    hookSocketPath: "/dev/null",
    sendBinaryUpstream: () => {},
    sendCellGridUpstream: (_ch, frame) => { frames.push(frame); },
  });
  return { mgr, frames };
}

async function injectCellSession(mgr: SessionManager, channelId: number): Promise<void> {
  const wtermCore = await WasmBridge.load();
  wtermCore.init(80, 24);
  (mgr as unknown as { sessions: Map<number, unknown> }).sessions.set(channelId, {
    sessionId: asSessionId("00000000-0000-0000-0000-000000000000"),
    channelId: asChannelId(channelId),
    socketPath: "/dev/null",
    kind: "shell" as const,
    cwd: "/",
    fsm: {} as never,
    bridge: null,
    scrollback: new Uint8Array(0),
    head_seq: 0,
    alt_mode: false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    wtermCore,
    cell_emit: initCellEmitState(),
  });
}

// emitUpstreamChunk is the PtyOut path: appendScrollback + sendBinaryUpstream +
// the coalesced cell schedule. appendScrollback alone does NOT schedule a cell
// emit, so drive the real chunk entrypoint.
const append = (mgr: SessionManager, ch: number, s: string): void =>
  (mgr as unknown as { emitUpstreamChunk(c: number, b: Buffer): void }).emitUpstreamChunk(ch, Buffer.from(s, "binary"));

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

describe("cell-delta coalescing (Phase-3)", () => {
  test("5 rapid chunks → 1 coalesced cell frame (not 5)", async () => {
    const { mgr, frames } = mgrWithCellCounter();
    await injectCellSession(mgr, 1);
    mgr.claimViewport(1, "v", 80, 24, 1, 1); frames.length = 0; // B: deltas only emit to a watched session
    for (let i = 0; i < 5; i++) append(mgr, 1, `line-${i}\r\n`);
    // Nothing emitted synchronously — the burst is absorbed.
    expect(frames.length).toBe(0);
    await sleep(40); // > CELL_EMIT_COALESCE_MS (16ms)
    expect(frames.length).toBe(1);
  });

  test("chunks in separate windows → separate frames", async () => {
    const { mgr, frames } = mgrWithCellCounter();
    await injectCellSession(mgr, 1);
    mgr.claimViewport(1, "v", 80, 24, 1, 1); frames.length = 0; // B: deltas only emit to a watched session
    append(mgr, 1, "first\r\n");
    await sleep(40);
    append(mgr, 1, "second\r\n");
    await sleep(40);
    expect(frames.length).toBe(2);
  });
});

// Seq-epoch reset on reload: a fresh page's per-mount claim counter resets to 1,
// colliding with the prior page's last seq (same stable viewer_key). The worker
// MUST still snapshot an INITIAL/TAB_VISIBLE re-claim or the reloaded viewer
// stays blank forever (deltas drop with no base) — the "refresh → nothing
// shows" bug. A heartbeat (VIEWPORT cause) on a stale seq must NOT re-snapshot.
describe("claim snapshot survives a seq-epoch reset (reload)", () => {
  test("INITIAL re-claim with a reset seq still emits a full snapshot", async () => {
    const { mgr, frames } = mgrWithCellCounter();
    await injectCellSession(mgr, 1);
    mgr.claimViewport(1, "viewer-A", 80, 24, 1, 1); // load A: seq=1, INITIAL
    const afterA = frames.length;
    expect(afterA).toBeGreaterThan(0);
    mgr.claimViewport(1, "viewer-A", 80, 24, 1, 1); // load B (reload): seq=1 again, stale
    expect(frames.length).toBeGreaterThan(afterA);  // snapshot emitted anyway
  });

  test("stale-seq heartbeat (VIEWPORT cause) does NOT re-snapshot (no spam)", async () => {
    const { mgr, frames } = mgrWithCellCounter();
    await injectCellSession(mgr, 1);
    mgr.claimViewport(1, "viewer-A", 80, 24, 1, 1); // INITIAL
    const afterInit = frames.length;
    mgr.claimViewport(1, "viewer-A", 80, 24, 1, 2); // heartbeat, same seq, VIEWPORT
    expect(frames.length).toBe(afterInit);          // no extra snapshot
  });
});

// B (draw only to attached clients): a session NO browser is viewing
// emits no per-chunk deltas — the grid still advances in wtermCore, and a viewer
// attaching re-claims → snapshot repaints it. Saves CPU+wire for background
// agents running unwatched.
describe("B: skip cell emit when nobody is watching", () => {
  test("unwatched → no delta; viewer attaches → snapshot; watched → deltas resume", async () => {
    const { mgr, frames } = mgrWithCellCounter();
    await injectCellSession(mgr, 1);
    for (let i = 0; i < 3; i++) append(mgr, 1, `bg-${i}\r\n`); // no viewer
    await sleep(40);
    expect(frames.length).toBe(0);                  // emitted to nobody → nothing
    mgr.claimViewport(1, "v", 80, 24, 1, 1);        // a viewer attaches
    expect(frames.length).toBe(1);                  // full snapshot catches it up
    append(mgr, 1, "fg\r\n");                        // now watched
    await sleep(40);
    expect(frames.length).toBe(2);                  // deltas flow again
  });
});
