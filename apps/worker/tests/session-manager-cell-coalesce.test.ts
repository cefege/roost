// Phase-3 (SSP rate governor): per-channel cell-delta coalescing. A burst
// of PtyOut chunks must emit ONE delta to the LATEST grid per coalesce window,
// not one frame per chunk — the wtermCore already holds every chunk's bytes, so
// the single coalesced read is "target = latest". Guards against reverting to a
// synchronous per-chunk emit (which floods the wire on resize storms / claude
// full-screen redraws).

import { describe, test, expect } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared";
import {
  initCellEmitState,
  SB_SNAPSHOT_HISTORY_ROWS,
  type CellGridFrame,
} from "@roost/shared/cell";
import { protoToCellFrame } from "@roost/shared/cell/cell-proto";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import { WasmBridge } from "@wterm/core";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";

function mgrWithCellCounter(): { mgr: SessionManager; frames: unknown[] } {
  const frames: unknown[] = [];
  const mgr = new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
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
    scrollback: createSbRing(),
    head_seq: 0,
    alt_mode: false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    ...initAgentOscState(),
    wtermCore,
    cell_emit: initCellEmitState("test-grid"),
    lastPtyOutMs: 0,
  });
}

function expectViewportOnlySnapshot(raw: unknown): CellGridFrame {
  const frame = protoToCellFrame(raw as PbCellGridFrame);
  expect(frame.full).toBe(true);
  expect(frame.scrollbackRows).toHaveLength(SB_SNAPSHOT_HISTORY_ROWS);
  expect(frame.sbBase).toBe(frame.scrollbackTotal);
  expect(frame.gridEpoch).toBe("test-grid:0");
  return frame;
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

  test("a continuously producing channel stays on ONE frame per coalesce window", async () => {
    // The trailing timer must RE-ARM while the channel keeps producing. Without
    // the re-arm the window closes after one flush and the next chunk starts a
    // fresh leading edge microseconds later: leading+trailing pairs, ~2x the
    // intended rate, and every extra frame is one scroll re-derive on every
    // viewer (the streaming-scroll zigzag).
    const { mgr, frames } = mgrWithCellCounter();
    await injectCellSession(mgr, 1);
    mgr.claimViewport(1, "v", 80, 24, 1, 1); frames.length = 0;
    const t0 = Date.now();
    while (Date.now() - t0 < 200) {
      append(mgr, 1, "streaming\r\n");
      await sleep(2);
    }
    const elapsed = Date.now() - t0;
    await sleep(40);
    // +2 slack: the leading-edge frame plus the final trailing flush.
    expect(frames.length).toBeLessThanOrEqual(Math.ceil(elapsed / 16) + 2);
    expect(frames.length).toBeGreaterThan(1); // frames really did flow
  });
});

// Seq-epoch reset on reload and applied-watermark heartbeat recovery share the
// stale-clientSeq branch: neither may regress dimensions or recompute SCD.
describe("stale-seq claim snapshot recovery", () => {
  test("INITIAL re-claim with a reset seq still emits a full snapshot", async () => {
    const { mgr, frames } = mgrWithCellCounter();
    await injectCellSession(mgr, 1);
    mgr.claimViewport(1, "viewer-A", 80, 24, 1, 1);
    const afterA = frames.length;
    expect(afterA).toBeGreaterThan(0);
    mgr.claimViewport(1, "viewer-A", 80, 24, 1, 1);
    expect(frames.length).toBeGreaterThan(afterA);
  });

  test("HEARTBEAT emits nothing when current and exactly one full snapshot when behind", async () => {
    const { mgr, frames } = mgrWithCellCounter();
    await injectCellSession(mgr, 1);
    mgr.claimViewport(1, "viewer-A", 80, 24, 1, 1);
    const held = protoToCellFrame(frames[0] as PbCellGridFrame).seq;

    mgr.claimViewport(1, "viewer-A", 200, 60, 1, 6, held);
    expect(frames.length).toBe(1);

    append(mgr, 1, "dropped-final-frame\r\n");
    await sleep(40);
    expect(frames.length).toBe(2);
    mgr.claimViewport(1, "viewer-A", 200, 60, 1, 6, held);
    expect(frames.length).toBe(3);
    expectViewportOnlySnapshot(frames[2]);

    const viewportState = mgr as unknown as {
      viewportClaims: Map<number, Map<string, { cols: number; rows: number }>>;
    };
    const claim = viewportState.viewportClaims.get(1)?.get("viewer-A");
    expect(claim).toMatchObject({ cols: 80, rows: 24 });
  });
});

describe("claimed-session resnapshot after coordinator reconnect", () => {
  test("emits exactly one full snapshot per live session with claims", async () => {
    const emittedChannels: number[] = [];
    const mgr = new SessionManager({
      workerFp: asWorkerFp("00".repeat(32)),
      sink: { emit: () => {} },
      sendBinaryUpstream: () => {},
      sendCellGridUpstream: (channelId) => { emittedChannels.push(channelId); },
    });
    await injectCellSession(mgr, 1);
    await injectCellSession(mgr, 2);
    await injectCellSession(mgr, 3);
    const claim = { cols: 80, rows: 24, lastMs: Date.now(), clientSeq: 1n };
    mgr.viewportClaims.set(1, new Map([["viewer-a", claim], ["viewer-b", claim]]));
    mgr.viewportClaims.set(2, new Map());
    mgr.viewportClaims.set(3, new Map([["viewer-c", claim]]));
    mgr.sessions.delete(3);

    mgr.resnapshotClaimedSessions();
    expect(emittedChannels).toEqual([1]);
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
    expectViewportOnlySnapshot(frames[0]);
    append(mgr, 1, "fg\r\n");                        // now watched
    await sleep(40);
    expect(frames.length).toBe(2);                  // deltas flow again
  });
});

// Every catch-up claim is viewport-only. The complete retained depth remains
// addressable through scrollbackTotal/sbBase and explicit history paging.
describe("claim snapshots never bundle retained history", () => {
  test("a 1200-row-deep catch-up claim reports depth with zero history rows", async () => {
    const { mgr, frames } = mgrWithCellCounter();
    await injectCellSession(mgr, 1);
    const core = mgr.shellByChannel(1)!.wtermCore;
    core.writeRaw(new TextEncoder().encode(
      Array.from({ length: 1200 }, (_, i) => `catchup-${i}`).join("\r\n") + "\r\n",
    ));
    const total = core.getScrollbackCount();
    expect(total).toBeGreaterThanOrEqual(1000);

    mgr.claimViewport(1, "v", 80, 24, 1, 1);
    expect(frames.length).toBe(1);
    const snapshot = expectViewportOnlySnapshot(frames[0]);
    expect(snapshot.scrollbackTotal).toBe(total);
  });
});

// held_cell_seq: the claimant reports the cell-frame seq it has already applied
// (SessionsResizeRequest.held_cell_seq → claimViewport's heldCellSeq), so a
// reveal of a pane that never stopped streaming repaints NOTHING. The seq is
// only proof while the channel stayed watched: with no claim, _hasActiveViewer
// gates emission off (session-emit.ts), so cell_emit.seq freezes while the grid
// keeps moving and a matching seq would prove the opposite of current.
describe("claim snapshot only when the claimant is not provably current", () => {
  test("re-claim holding the last emitted seq emits nothing; one behind emits a full frame", async () => {
    const { mgr, frames } = mgrWithCellCounter();
    await injectCellSession(mgr, 1);
    mgr.claimViewport(1, "v", 80, 24, 1, 1);
    expect(frames.length).toBe(1);
    const held = protoToCellFrame(frames[0] as PbCellGridFrame).seq;

    // Reveal of a pane that is current: TAB_VISIBLE, held seq matches.
    mgr.claimViewport(1, "v", 80, 24, 2, 3, held);
    expect(frames.length).toBe(1);

    // One frame behind → catch-up snapshot.
    mgr.claimViewport(1, "v", 80, 24, 3, 3, held - 1);
    expect(frames.length).toBe(2);
    expectViewportOnlySnapshot(frames[1]);
  });

  test("a re-subscribing background claim catches up; a current one stays silent", async () => {
    const { mgr, frames } = mgrWithCellCounter();
    await injectCellSession(mgr, 1);
    mgr.claimViewport(1, "v", 80, 24, 1, 1);
    expect(frames.length).toBe(1);
    mgr.withdrawViewport(1, "v");
    await sleep(1000); // > VIEWER_WITHDRAW_GRACE_MS (800) — the claim is gone

    // Deck LRU re-promotes this pane: BACKGROUND, holds nothing.
    mgr.claimViewport(1, "v", 0, 0, 2, 5, 0);
    expect(frames.length).toBe(2);
    const caught = expectViewportOnlySnapshot(frames[1]);

    // Still subscribed and current → no repaint.
    mgr.claimViewport(1, "v", 0, 0, 3, 5, caught.seq);
    expect(frames.length).toBe(2);
  });

  test("output produced while UNWATCHED still forces a snapshot on the matching seq", async () => {
    // cell_emit.seq freezes while nobody claims, so held === cell_emit.seq says
    // nothing about the grid. Without the wasStreaming clause the returning
    // pane would paint its pre-withdraw bottom until the next PTY chunk.
    const { mgr, frames } = mgrWithCellCounter();
    await injectCellSession(mgr, 1);
    mgr.claimViewport(1, "v", 80, 24, 1, 1);
    const held = protoToCellFrame(frames[0] as PbCellGridFrame).seq;
    mgr.withdrawViewport(1, "v");
    await sleep(1000);

    append(mgr, 1, "finished-while-away\r\n");
    await sleep(40);
    expect(frames.length).toBe(1);                 // unwatched → nothing emitted
    expect(mgr.shellByChannel(1)!.cell_emit.seq).toBe(held); // seq really froze

    mgr.claimViewport(1, "v", 80, 24, 2, 3, held);
    expect(frames.length).toBe(2);
    const revealed = expectViewportOnlySnapshot(frames[1]);
    expect(revealed.viewportRows.some((r) =>
      r.spans.map((s) => s.text).join("").includes("finished-while-away"),
    )).toBe(true);
  });
});

// __roostLag()'s worker_prep segment is (workerEmitMs - ptyOutMs). Stamping both
// from one Date.now() at emit made that leg structurally zero, hiding the whole
// coalesce window plus grid read. ptyOutMs must date the OLDEST unshipped byte
// of the burst, and must be released once shipped so the next burst re-dates it.
describe("ptyOutMs dates the oldest unshipped byte, not the emit", () => {
  test("a chunk absorbed by the trailing window is dated from its arrival", async () => {
    const { mgr, frames } = mgrWithCellCounter();
    await injectCellSession(mgr, 1);
    mgr.claimViewport(1, "v", 80, 24, 1, 1); frames.length = 0;
    append(mgr, 1, "first\r\n");
    await sleep(0);                     // leading-edge microtask fires, arms the window
    expect(frames.length).toBe(1);
    frames.length = 0;                  // the leading edge waited for nothing
    const arrived = Date.now();
    append(mgr, 1, "second\r\n");       // absorbed: waits out CELL_EMIT_COALESCE_MS
    const appendReturned = Date.now();
    await sleep(60);
    expect(frames.length).toBe(1);
    const trailing = frames[0] as PbCellGridFrame;
    // Dated from arrival, NOT from emit: emit is a coalesce window later, so an
    // emit-time stamp could not fall inside the append's own instant.
    expect(Number(trailing.ptyOutMs)).toBeGreaterThanOrEqual(arrived);
    expect(Number(trailing.ptyOutMs)).toBeLessThanOrEqual(appendReturned);
    expect(trailing.workerEmitMs).toBeGreaterThanOrEqual(trailing.ptyOutMs);
    // Released after shipping, so the next burst is dated from its own arrival.
    expect(mgr.shellByChannel(1)!.lastPtyOutMs).toBe(0);
  });
});
