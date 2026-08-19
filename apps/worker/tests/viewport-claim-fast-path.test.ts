// What a terminal reveal is ALLOWED TO COST. Two independent gates decide that,
// and each one shipped as a seconds-long stall whose pane visibly reloaded its
// scrollback:
//
//   the resize floor   an invalid floor made a claim at BYTE-IDENTICAL geometry
//                      fall into the full transaction, which freezes the live
//                      core and rebuilds it — minting a new gridEpoch and
//                      throwing the browser's painted history away. The floor
//                      gates only ALLOCATING a resize sequence, so a claim that
//                      issues no resize needs none: it re-READs the keeper
//                      (session-resize-floor.ts::revalidateResizeFloor).
//   the claim snapshot every reveal past the 800 ms withdraw grace repainted,
//                      because a frozen cell_emit.seq proves nothing on its own
//                      once emission is gated off. rec.lastPtyOutMs is the
//                      missing witness (session-viewport.ts::needsClaimSnapshot).
//
// Only the keeper SOCKET is faked (keeper-fake-pool.ts), so admission, the
// per-command watchdogs and the transaction's phase deadlines are production
// code: a duration asserted here is a duration the user waits out.

import { describe, test, expect, afterEach } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared/wire";
import { initCellEmitState } from "@roost/shared/cell";
import { protoToCellFrame } from "@roost/shared/cell/cell-proto";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import { WasmBridge } from "@wterm/core";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";
import { MuxFrameType } from "../src/keeper/protocol.ts";
import { getMultiplexedPool } from "../src/keeper/multiplexed-client.ts";
import type { MultiplexedKeeperPool } from "../src/keeper/multiplexed-client.ts";
import {
  rebuildTerminalCore,
  VIEWPORT_TXN_BUDGET_MS,
  type ResizeCapture,
} from "../src/session-resize-capture.ts";
import { FLOOR_REVALIDATE_BUDGET_MS } from "../src/session-resize-floor.ts";
import { installFakeKeeper, installAutoKeeper, type FakeKeeper } from "./keeper-fake-pool.ts";
import type { WorkerViewportIntent } from "../src/session-terminal-control.ts";

const SESSION_ID = asSessionId("6b3c1d40-2222-4333-8444-555566667777");
const CHANNEL_ID = 11;
const VIEWER = "viewer:tab-a";
const COLS = 80;
const ROWS = 24;
/** numeric roost.v1.ResizeCause — the browser event behind a claim. */
const VIEWPORT = 2;
const TAB_VISIBLE = 3;

/** REAL elapsed time, deliberately. Every deadline under test is production
 *  wall-clock — the deferred withdraw's 800 ms grace, the keeper's 2.5 s command
 *  watchdogs, the capture's phase budgets, and the 750 ms floor probe — and one
 *  case ASSERTS a measured duration. Faking the clock would replace the subject
 *  of the test with the test's own timer mock, and there is no earlier signal to
 *  await: "the grace elapsed with no re-claim" is defined by the clock. */
function sleep(ms: number): Promise<void> {
  const { promise, resolve } = Promise.withResolvers<void>();
  setTimeout(resolve, ms);
  return promise;
}

/** Let queued microtasks/promise chains run without advancing timers. */
async function settle(rounds = 6): Promise<void> {
  for (let i = 0; i < rounds; i++) await Promise.resolve();
}

interface Fixture {
  mgr: SessionManager;
  keeper: FakeKeeper;
  cells: PbCellGridFrame[];
  /** How many times the live core was FROZEN, counted at the map write.
   *  installResizeCapture is the only writer of `resizeCaptures`, and the
   *  transaction's `finally` clears the entry on every path — so a `has()`
   *  sampled after the await can never see the freeze a fast path is supposed
   *  to have avoided. Counting the write can. */
  coreFreezes(): number;
  dispose(): void;
}

let live: Fixture | null = null;

afterEach(() => {
  live?.dispose();
  live = null;
});

async function fixture(opts: { keeper: "answers" | "silent" }): Promise<Fixture> {
  const cells: PbCellGridFrame[] = [];
  // Before the manager: its constructor calls pool.ensure(), and a real keeper
  // adopted by that dial would replace the fake socket mid-test. "silent" is a
  // keeper that never answers ANY frame — unreachable or wedged.
  const keeper = opts.keeper === "answers"
    ? installAutoKeeper({ cols: COLS, rows: ROWS })
    : installFakeKeeper();
  const mgr = new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
    sendBinaryUpstream: () => "sent",
    sendCellGridUpstream: (_channelId, frame) => {
      cells.push(frame);
      return "sent";
    },
  });
  const wtermCore = await WasmBridge.load();
  wtermCore.init(COLS, ROWS);
  const seedBytes = new TextEncoder().encode("prompt$ ");
  wtermCore.writeRaw(seedBytes);
  mgr.sessions.set(CHANNEL_ID, {
    sessionId: SESSION_ID,
    channelId: asChannelId(CHANNEL_ID),
    socketPath: "/dev/null",
    kind: "shell" as const,
    cwd: "/",
    fsm: {} as never,
    scrollback: createSbRing(seedBytes),
    head_seq: seedBytes.byteLength,
    alt_mode: false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    query_carry: new Uint8Array(0),
    ...initAgentOscState(),
    wtermCore,
    cell_emit: initCellEmitState("fastpath-grid"),
    lastPtyOutMs: 0,
    sb_origin_pin: null,
    spawnedAtMs: Date.now(),
    session_trace_id: "trace-fastpath",
  } as never);
  // The seeds every real creation path records (session-spawn.ts, and now
  // respawn): the keeper made this PTY at exactly COLS×ROWS, and no sequenced
  // resize has been applied on the channel. Without them floorValid() is false.
  mgr.lastAppliedSize.set(CHANNEL_ID, { cols: COLS, rows: ROWS });
  mgr.channelResizeSeq.set(CHANNEL_ID, 0);
  let freezes = 0;
  const writeCapture = mgr.resizeCaptures.set.bind(mgr.resizeCaptures);
  const captureWrites: { set: Map<number, ResizeCapture>["set"] } = mgr.resizeCaptures;
  captureWrites.set = (channelId, capture) => {
    freezes++;
    return writeCapture(channelId, capture);
  };
  const result: Fixture = {
    mgr,
    keeper,
    cells,
    coreFreezes: () => freezes,
    dispose: () => {
      keeper.restore();
      // Drop through the real close path: a respawned record owns a ports poll
      // and a git watch that a bare sessions.delete() would leak.
      for (const channelId of [...mgr.sessions.keys()]) mgr._dropChannelState(channelId);
      mgr.dispose();
    },
  };
  live = result;
  return result;
}

function claim(overrides: Partial<WorkerViewportIntent> & { clientSeq: bigint }): WorkerViewportIntent {
  return {
    sessionId: SESSION_ID,
    viewerId: VIEWER,
    cause: TAB_VISIBLE,
    heldCellSeq: 0n,
    cols: COLS,
    rows: ROWS,
    ...overrides,
  };
}

// The floor is a statement about SEQUENCE ALLOCATION, not about geometry. These
// three cases pin both sides of that: a claim that allocates nothing never pays
// for an unproven floor, and a claim that does still pays in full.
describe("a claim at unchanged geometry never freezes the core", () => {
  test("an invalid floor does not rebuild the core for an unchanged viewport", async () => {
    const f = await fixture({ keeper: "answers" });
    // Exactly what one keeper hiccup leaves behind, and what used to make every
    // LATER switch to this session slow: the floor is unproven. The geometry is
    // not — lastAppliedSize still names the size the claim is about to ask for.
    f.mgr.resizeFloorInvalid.add(CHANNEL_ID);

    const result = await f.mgr.applyTerminalViewport(claim({ clientSeq: 1n }));

    expect(result).toMatchObject({ status: "committed", cols: COLS, rows: ROWS, resized: false });
    // The point of the whole change: no capture was ever installed, so the core
    // never stopped parsing, no rebuild ran, and the browser's grid identity
    // survives — a fresh gridEpoch is what discards its painted history.
    expect(f.coreFreezes()).toBe(0);
    expect(f.mgr.coreRebuilds.get(CHANNEL_ID)).toBeUndefined();
    expect(protoToCellFrame(f.cells.at(-1)!).gridEpoch).toBe("fastpath-grid:0");
    // One capture-free READ, and no resize: the keeper's geometry is unchanged.
    expect(f.keeper.order()).toEqual(["GetTerminalState"]);
    // That read also PROVED the floor, so the next switch skips even the read.
    expect(f.mgr.resizeFloorInvalid.has(CHANNEL_ID)).toBe(false);
  });

  test("an unprovable floor still falls through to the full transaction", async () => {
    const f = await fixture({ keeper: "silent" });
    f.mgr.resizeFloorInvalid.add(CHANNEL_ID);

    const startedMs = Date.now();
    const result = await f.mgr.applyTerminalViewport(claim({ clientSeq: 1n }));
    const elapsedMs = Date.now() - startedMs;

    // Unprovable is never treated as proven. The capture-free read times out at
    // its own ceiling and the claim then takes TODAY's conservative path —
    // freeze, reconcile, rebuild exactly once — instead of committing a
    // geometry nothing confirmed.
    expect(elapsedMs).toBeGreaterThanOrEqual(FLOOR_REVALIDATE_BUDGET_MS);
    expect(f.coreFreezes()).toBe(1);
    expect(f.mgr.coreRebuilds.get(CHANNEL_ID)).toBe(1);
    expect(result.status).toBe("committed");
    // And the extra read is BOUNDED: it sits under the keeper's own 2.5 s
    // command watchdog, so the fall-through still lands inside the transaction
    // ceiling the coordinator is waiting on rather than extending past it.
    expect(elapsedMs).toBeLessThan(VIEWPORT_TXN_BUDGET_MS);
  }, { timeout: 30_000 });

  test("a genuine resize still rebuilds exactly once", async () => {
    const f = await fixture({ keeper: "answers" });

    const result = await f.mgr.applyTerminalViewport(
      claim({ clientSeq: 1n, cause: VIEWPORT, cols: 100, rows: 30 }),
    );

    // The anti-over-application control for the first case: the fast path is a
    // SIZE test, not a blanket "claims never resize". A real geometry change
    // still allocates one sequence, freezes once, and rebuilds once at the size
    // the keeper ACKed — and the core the browser paints must BE that size.
    expect(result).toEqual({
      status: "committed", channelResizeSeq: 1, cols: 100, rows: 30, resized: true,
    });
    expect(f.keeper.seqOf(MuxFrameType.ResizeRequest)).toEqual([1]);
    expect(f.coreFreezes()).toBe(1);
    expect(f.mgr.coreRebuilds.get(CHANNEL_ID)).toBe(1);
    const core = f.mgr.sessions.get(CHANNEL_ID)!.wtermCore;
    expect([core.getCols(), core.getRows()]).toEqual([100, 30]);
  });
});

// needsClaimSnapshot's two non-seq witnesses, each isolated so the ONLY thing
// differing between a silent reveal and a repainting one is the witness under
// test. A hidden pane keeps sending WITHDRAW (docs/FAILURE-INDEX.md — "Reveal
// after dormancy re-dials or re-streams"), so dormancy is the normal case here,
// not an edge one.
describe("a reveal repaints only when a witness says the grid moved", () => {
  test("a reveal after dormancy emits nothing when no PTY byte landed", async () => {
    const f = await fixture({ keeper: "answers" });
    f.mgr.claimViewport(CHANNEL_ID, VIEWER, COLS, ROWS, 1, TAB_VISIBLE);
    expect(f.cells.length).toBe(1);
    const held = protoToCellFrame(f.cells[0]!).seq;

    // Park the pane and let the DEFERRED withdraw really fire. Emission is now
    // gated off, so cell_emit.seq freezes and stops being proof by itself.
    f.mgr.withdrawViewport(CHANNEL_ID, VIEWER);
    await sleep(1000); // > VIEWER_WITHDRAW_GRACE_MS (800) — the claim is gone
    const rec = f.mgr.shellByChannel(CHANNEL_ID)!;
    expect(rec.cell_emit.seq).toBe(held);
    // THE witness: emitCellFrame zeroes lastPtyOutMs on every SUCCESSFUL send
    // and emitUpstreamChunk stamps it on the first chunk after that, so 0 says
    // the grid cannot have moved since the claimant's frame shipped.
    expect(rec.lastPtyOutMs).toBe(0);

    f.mgr.claimViewport(CHANNEL_ID, VIEWER, COLS, ROWS, 2, TAB_VISIBLE, held);
    await settle();
    // Zero bytes, zero repaint: a shell sitting at a prompt reveals for free.
    expect(f.cells.length).toBe(1);

    // Converse, same channel, same held seq: one byte lands while unwatched and
    // the witness flips. Without it the reveal above would be a silent bug.
    f.mgr.withdrawViewport(CHANNEL_ID, VIEWER);
    await sleep(1000);
    f.mgr.emitUpstreamChunk(CHANNEL_ID, Buffer.from("build finished\r\n", "binary"));
    await sleep(40); // past the coalesce window: unwatched, so nothing ships
    expect(f.cells.length).toBe(1);
    expect(rec.cell_emit.seq).toBe(held); // the seq still matches, and still lies
    expect(rec.lastPtyOutMs).not.toBe(0);

    f.mgr.claimViewport(CHANNEL_ID, VIEWER, COLS, ROWS, 3, TAB_VISIBLE, held);
    expect(f.cells.length).toBe(2);
    const revealed = protoToCellFrame(f.cells[1]!);
    expect(revealed.full).toBe(true);
    expect(revealed.viewportRows.some(
      (row) => row.spans.map((span) => span.text).join("").includes("build finished"),
    )).toBe(true);
  }, { timeout: 30_000 });

  test("a rebuilt core always resnapshots even at a matching seq", async () => {
    const f = await fixture({ keeper: "answers" });
    f.mgr.claimViewport(CHANNEL_ID, VIEWER, COLS, ROWS, 1, TAB_VISIBLE);
    const first = protoToCellFrame(f.cells[0]!);

    // Control: the claim never stopped streaming and the held seq matches, so
    // this reveal is silent. Exactly one thing changes below.
    f.mgr.claimViewport(CHANNEL_ID, VIEWER, COLS, ROWS, 2, TAB_VISIBLE, first.seq);
    expect(f.cells.length).toBe(1);

    // A null capture takes rebuildTerminalCore's same-size fast-skip, so the
    // size must differ for a real swap. The swap KEEPS cell_emit.seq (the SPA's
    // gap detector must see no rewind) and mints a new gridEpoch, so from here
    // the held seq names a grid that no longer exists.
    await rebuildTerminalCore(f.mgr, CHANNEL_ID, COLS + 20, ROWS, null);
    const rec = f.mgr.shellByChannel(CHANNEL_ID)!;
    expect(rec.cell_emit.seq).toBe(first.seq);
    expect(rec.lastPtyOutMs).toBe(0);

    f.mgr.claimViewport(CHANNEL_ID, VIEWER, COLS, ROWS, 3, TAB_VISIBLE, first.seq);
    expect(f.cells.length).toBe(2);
    const resnapshot = protoToCellFrame(f.cells[1]!);
    // cell_emit.sentFull is the only witness that failed — same seq, same live
    // claim, no PTY byte — and it is what stops the browser from applying
    // deltas onto a dead epoch. A full frame is the only safe answer.
    expect(resnapshot.full).toBe(true);
    expect(resnapshot.gridEpoch).not.toBe(first.gridEpoch);
  });
});

// spawnShell records both floor seeds; respawn() allocates a brand-new channel
// id and used to record neither, so a respawned session cold-pathed its first
// reveal — for the rest of the worker's life.
describe("respawn", () => {
  test("respawn seeds proven geometry", async () => {
    const f = await fixture({ keeper: "answers" });
    // The fake socket answers FRAMES, not spawns. Stub exactly the one call
    // that needs a real keeper child; record creation, the wterm core build and
    // the seeding under test all run for real. Not a cast: the pool structurally
    // has this member, and the alias only makes the property assignable.
    const pool = getMultiplexedPool();
    const spawnSeam: { spawn: MultiplexedKeeperPool["spawn"] } = pool;
    const priorSpawn = spawnSeam.spawn;
    spawnSeam.spawn = async () => 4242;
    try {
      await f.mgr.respawn({ oldSessionId: SESSION_ID, cwd: "/tmp", kind: "shell", cols: 100, rows: 30 });
    } finally {
      spawnSeam.spawn = priorSpawn;
    }

    const respawned = f.mgr.getBySessionId(SESSION_ID)!;
    expect(respawned.channelId).not.toBe(CHANNEL_ID);
    // 0 is not "unknown": the channel id is BRAND NEW, so the keeper has never
    // applied a sequenced resize on it and the next allocation is legitimately
    // 1. Recording it is what makes floorValid() true — an absent entry reads
    // as a channel this worker merely adopted, whose floor cannot be trusted.
    expect(f.mgr.channelResizeSeq.get(respawned.channelId)).toBe(0);
    // The keeper created THIS PTY at exactly 100×30, so that is proven applied
    // geometry, not a guess.
    expect(f.mgr.lastAppliedSize.get(respawned.channelId)).toEqual({ cols: 100, rows: 30 });

    // What those two entries actually buy, end to end: the first reveal of a
    // respawned pane commits with no keeper round trip and no core rebuild.
    const revealed = await f.mgr.applyTerminalViewport({
      sessionId: SESSION_ID, viewerId: VIEWER, cause: TAB_VISIBLE,
      heldCellSeq: 0n, clientSeq: 1n, cols: 100, rows: 30,
    });
    expect(revealed).toMatchObject({ status: "committed", cols: 100, rows: 30, resized: false });
    expect(f.mgr.coreRebuilds.get(respawned.channelId)).toBeUndefined();
    expect(f.keeper.seqOf(MuxFrameType.ResizeRequest)).toEqual([]);
    expect(f.keeper.order()).not.toContain("GetTerminalState");
  }, { timeout: 30_000 });
});
