// The viewport transaction as a state machine over a real two-phase keeper
// client. Every case here is a failure that shipped as "the terminal stopped
// streaming": a lost ACK that fabricated a rejection, post-resize bytes parsed at
// the old width, input stuck behind a resize round trip, and two owners building
// two cores for one decision.
//
// Only the keeper SOCKET is faked (keeper-fake-pool.ts), so admission, the
// per-command watchdogs, and the synchronous result-frame boundary hook are the
// production code paths.

import { describe, test, expect, afterEach } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared/wire";
import { gridToCellFrame, initCellEmitState } from "@roost/shared/cell";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import { WasmBridge } from "@wterm/core";
import { createSbRing, readRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";
import { MuxFrameType } from "../src/keeper/protocol.ts";
import { KeeperFeature } from "../src/keeper/protocol.ts";
import { installFakeKeeper, type FakeKeeper } from "./keeper-fake-pool.ts";
import type { WorkerViewportIntent } from "../src/session-terminal-control.ts";

const SESSION_ID = asSessionId("2f9d4a10-1111-4222-8333-444455556666");
const CHANNEL_ID = 7;
const VIEWER = "viewer:tab-a";
/** DEC private mode 2026 opener, unclosed on purpose: a TUI mid-repaint. */
const SYNC_ON = "\x1b[?2026h";
const SYNC_OFF = "\x1b[?2026l";

interface Fixture {
  mgr: SessionManager;
  keeper: FakeKeeper;
  cells: PbCellGridFrame[];
  dispose(): void;
}

let live: Fixture | null = null;

afterEach(() => {
  live?.dispose();
  live = null;
});

async function fixture(opts: {
  cols: number;
  rows: number;
  seed?: string;
  features?: readonly string[];
}): Promise<Fixture> {
  const cells: PbCellGridFrame[] = [];
  // Before the manager: its constructor calls pool.ensure(), and a real keeper
  // adopted by that dial would replace the fake socket mid-test.
  const keeper = installFakeKeeper({ features: opts.features });
  const mgr = new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
    sendBinaryUpstream: () => "sent",
    sendCellGridUpstream: (_channelId, frame) => {
      cells.push(frame);
      return "sent";
    },
  });
  const seed = opts.seed ?? "";
  const wtermCore = await WasmBridge.load();
  wtermCore.init(opts.cols, opts.rows);
  const seedBytes = new TextEncoder().encode(seed);
  if (seed.length > 0) wtermCore.writeRaw(seedBytes);
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
    cell_emit: initCellEmitState("txn-grid"),
    lastPtyOutMs: 0,
    spawnedAtMs: Date.now(),
    session_trace_id: "trace-txn",
  } as never);
  mgr.lastAppliedSize.set(CHANNEL_ID, { cols: opts.cols, rows: opts.rows });
  mgr.channelResizeSeq.set(CHANNEL_ID, 0);
  const result: Fixture = {
    mgr,
    keeper,
    cells,
    dispose: () => {
      keeper.restore();
      mgr.sessions.delete(CHANNEL_ID);
      mgr.dispose();
    },
  };
  live = result;
  return result;
}

function claim(overrides: Partial<WorkerViewportIntent> & { clientSeq: bigint; cols: number; rows: number }): WorkerViewportIntent {
  return {
    sessionId: SESSION_ID,
    viewerId: VIEWER,
    cause: 2,
    heldCellSeq: 0n,
    ...overrides,
  };
}

/** Deliver a PTY chunk exactly where the inbound dispatcher does. */
function ptyOut(mgr: SessionManager, text: string): void {
  mgr.emitUpstreamChunk(CHANNEL_ID, Buffer.from(text, "binary"));
}

function coreText(mgr: SessionManager): string {
  const core = mgr.sessions.get(CHANNEL_ID)!.wtermCore;
  const frame = gridToCellFrame(core, 0, "txn-grid:0");
  const lines: string[] = [];
  for (const row of frame.scrollbackRows) lines.push(row.spans.map((s) => s.text).join(""));
  for (const row of frame.viewportRows) lines.push(row.spans.map((s) => s.text).join(""));
  return lines.join("\n").replace(/\s+$/g, "");
}

function retained(mgr: SessionManager): string {
  return new TextDecoder().decode(readRing(mgr.sessions.get(CHANNEL_ID)!.scrollback));
}

/** Let queued microtasks/promise chains run without advancing timers. */
async function settle(rounds = 6): Promise<void> {
  for (let i = 0; i < rounds; i++) await Promise.resolve();
}

describe("viewport transaction stages", () => {
  test("a proven resize commits, applies captured output once, and rebuilds exactly one core", async () => {
    const f = await fixture({ cols: 80, rows: 24, seed: "before\r\n" });
    const applied = f.mgr.applyTerminalViewport(claim({ clientSeq: 1n, cols: 100, rows: 30 }));
    await settle();

    const request = f.keeper.writes.find((w) => w.type === MuxFrameType.ResizeRequest);
    expect(request).toMatchObject({ seq: 1, cols: 100, rows: 30 });
    // The capture is installed BEFORE the first keeper operation, so a chunk that
    // arrives while the geometry is unproven never reaches the frozen core.
    ptyOut(f.mgr, "during\r\n");
    expect(coreText(f.mgr)).toBe("before");

    // The boundary is a property of the keeper's ORDERED stream. Assert it is
    // stamped INSIDE the result-frame settle, with no await in between: deferring
    // it by even one microtask is what let post-resize bytes be attributed to the
    // pre-resize geometry (and, on alt-screen, replayed at the wrong width).
    const capture = f.mgr.resizeCaptures.get(CHANNEL_ID)!;
    expect(capture.boundarySeq).toBe(-1);
    const headAtAck = f.mgr.sessions.get(CHANNEL_ID)!.head_seq;
    f.keeper.resizeAck(CHANNEL_ID, 1, 100, 30);
    expect(capture.boundarySeq).toBe(headAtAck);
    ptyOut(f.mgr, "after\r\n");
    const result = await applied;

    expect(result).toEqual({
      status: "committed",
      channelResizeSeq: 1,
      cols: 100,
      rows: 30,
      resized: true,
    });
    expect(f.mgr.coreRebuilds.get(CHANNEL_ID)).toBe(1);
    expect(f.mgr.resizeCaptures.has(CHANNEL_ID)).toBe(false);
    expect(f.mgr.cellEmissionGates.has(CHANNEL_ID)).toBe(false);
    expect(retained(f.mgr)).toBe("before\r\nduring\r\nafter\r\n");
    // Applied once, in order, at the proven geometry — not duplicated by the
    // rebuild and not lost by the freeze.
    expect(coreText(f.mgr)).toBe("before\nduring\nafter");
    const core = f.mgr.sessions.get(CHANNEL_ID)!.wtermCore;
    expect([core.getCols(), core.getRows()]).toEqual([100, 30]);
    const published = f.cells.at(-1)!;
    expect(published.full).toBe(true);
    expect([published.cols, published.rows]).toEqual([100, 30]);
  });

  test("repeated unknown results converge to a bounded ambiguous result and free the channel", async () => {
    const f = await fixture({ cols: 80, rows: 24, seed: "seed\r\n" });
    const applied = f.mgr.applyTerminalViewport(claim({ clientSeq: 1n, cols: 100, rows: 30 }));
    await settle();
    // Every attempt answers "unknown": the keeper may or may not have resized.
    for (let i = 0; i < 12; i++) {
      f.keeper.resizeUnknown(CHANNEL_ID, 1);
      await settle();
    }
    const result = await applied;

    expect(result.status).toBe("ambiguous");
    // Finite attempts: one request plus the bounded status probes, never a spin.
    expect(f.keeper.seqOf(MuxFrameType.ResizeRequest)).toEqual([1]);
    // Exactly the finite attempt budget: the bound is reached, not sidestepped.
    expect(f.keeper.seqOf(MuxFrameType.ResizeStatus)).toEqual([1, 1, 1]);
    expect(f.mgr.cellEmissionGates.has(CHANNEL_ID)).toBe(false);
    expect(f.mgr.resizeCaptures.has(CHANNEL_ID)).toBe(false);
    // Ambiguity invalidates the floor and leaves a repair marker instead of a
    // guessed size.
    expect(f.mgr.resizeFloorInvalid.has(CHANNEL_ID)).toBe(true);
    expect(retained(f.mgr)).toBe("seed\r\n");

    // The next input is admitted immediately, with no leftover lane holder.
    const wrote = f.mgr.writeTerminalInput(SESSION_ID, 1n, new TextEncoder().encode("x"));
    expect(await f.keeper.waitForWrite(MuxFrameType.PtyInRequest)).toMatchObject({ seq: 1 });
    f.keeper.inputAck(CHANNEL_ID, 1, 1);
    expect(await wrote).toEqual({ status: "accepted", writtenBytes: 1 });
  });

  test("a lost ACK recovers the authoritative sequence and size without repeating the resize", async () => {
    const f = await fixture({ cols: 80, rows: 24, seed: "A" });
    const lost = f.mgr.applyTerminalViewport(claim({ clientSeq: 1n, cols: 100, rows: 30 }));
    await settle();
    ptyOut(f.mgr, "B");
    // The keeper applied seq 1 and its ACK never arrived.
    for (let i = 0; i < 6; i++) {
      f.keeper.resizeUnknown(CHANNEL_ID, 1);
      await settle();
    }
    expect((await lost).status).toBe("ambiguous");
    const afterLoss = f.keeper.writes.length;

    // The next claim recovers authority first. appliedResizeSeq proves seq 1
    // landed, and its geometry already matches the newest desired size.
    const converged = f.mgr.applyTerminalViewport(claim({ clientSeq: 2n, cols: 100, rows: 30 }));
    await settle();
    f.keeper.terminalState(CHANNEL_ID, {
      headSeq: 2,
      cols: 100,
      rows: 30,
      highestResizeSeq: 1,
      appliedResizeSeq: 1,
    });
    const result = await converged;

    expect(result).toMatchObject({ status: "committed", channelResizeSeq: 1, cols: 100, rows: 30 });
    // No duplicate resize: only the terminal-state query went out.
    expect(f.keeper.seqOf(MuxFrameType.ResizeRequest)).toEqual([1]);
    expect(f.keeper.writes.slice(afterLoss).map((w) => w.type))
      .toEqual([MuxFrameType.GetTerminalState]);
    expect(f.mgr.lastAppliedSize.get(CHANNEL_ID)).toEqual({ cols: 100, rows: 30 });
    expect(f.mgr.resizeFloorInvalid.has(CHANNEL_ID)).toBe(false);
    // A truthful tail: every retained byte, once, in order.
    expect(retained(f.mgr)).toBe("AB");
    expect(coreText(f.mgr)).toBe("AB");

    // A genuinely different size is issued as N+1, never as a conflicting N.
    const grown = f.mgr.applyTerminalViewport(claim({ clientSeq: 3n, cols: 120, rows: 40 }));
    await settle();
    const next = f.keeper.writes.filter((w) => w.type === MuxFrameType.ResizeRequest).at(-1)!;
    expect(next).toMatchObject({ seq: 2, cols: 120, rows: 40 });
    f.keeper.resizeAck(CHANNEL_ID, 2, 120, 40);
    expect((await grown).status).toBe("committed");
  });

  test("input queued during a pending resize is written after it, without waiting for the ACK", async () => {
    const f = await fixture({ cols: 80, rows: 24 });
    const applied = f.mgr.applyTerminalViewport(claim({ clientSeq: 1n, cols: 100, rows: 30 }));
    const wrote = f.mgr.writeTerminalInput(SESSION_ID, 1n, new TextEncoder().encode("ls\r"));
    await settle();

    // Receive order preserved at the keeper, and the input is already written
    // while the resize is still unresolved.
    expect(f.keeper.order()).toEqual(["ResizeRequest", "PtyInRequest"]);
    let viewportSettled = false;
    void applied.then(() => { viewportSettled = true; });
    await settle();
    expect(viewportSettled).toBe(false);

    f.keeper.inputAck(CHANNEL_ID, 1, 3);
    expect(await wrote).toEqual({ status: "accepted", writtenBytes: 3 });
    // Exactly one input write: a pending control never causes a retry.
    expect(f.keeper.writes.filter((w) => w.type === MuxFrameType.PtyInRequest)).toHaveLength(1);

    f.keeper.resizeAck(CHANNEL_ID, 1, 100, 30);
    expect((await applied).status).toBe("committed");
  });

  test("an invalid floor probes status, issues N+1, then releases input in keeper order", async () => {
    // No terminal-state feature: a deployed keeper's recovery path is the status
    // probe, which by itself must never satisfy resize admission.
    const f = await fixture({
      cols: 80,
      rows: 24,
      seed: "old\r\n",
      features: [KeeperFeature.OrderedHistory, KeeperFeature.AcknowledgedInput, KeeperFeature.AcknowledgedResize],
    });
    f.mgr.channelResizeSeq.set(CHANNEL_ID, 5);
    f.mgr.resizeFloorInvalid.add(CHANNEL_ID);

    const applied = f.mgr.applyTerminalViewport(claim({ clientSeq: 1n, cols: 100, rows: 30 }));
    const wrote = f.mgr.writeTerminalInput(SESSION_ID, 9n, new TextEncoder().encode("q"));
    await settle();
    expect(f.keeper.order()).toEqual(["ResizeStatus"]);

    // Status says seq 5 applied an OLD geometry, so the newest desired size still
    // needs N+1 — and only that write may release the input behind it.
    f.keeper.resizeAck(CHANNEL_ID, 5, 90, 26);
    await settle();
    expect(f.keeper.order()).toEqual(["ResizeStatus", "ResizeRequest", "PtyInRequest"]);
    expect(f.keeper.writes[1]).toMatchObject({ seq: 6, cols: 100, rows: 30 });

    // Concurrent output during the whole exchange stays out of the old core.
    ptyOut(f.mgr, "new\r\n");
    expect(coreText(f.mgr)).toBe("old");

    f.keeper.resizeAck(CHANNEL_ID, 6, 100, 30);
    ptyOut(f.mgr, "tail\r\n");
    const result = await applied;
    expect(result).toMatchObject({ status: "committed", channelResizeSeq: 6, cols: 100, rows: 30 });
    expect(retained(f.mgr)).toBe("old\r\nnew\r\ntail\r\n");
    expect(coreText(f.mgr)).toBe("old\nnew\ntail");
    const published = f.cells.at(-1)!;
    expect([published.cols, published.rows]).toEqual([100, 30]);

    f.keeper.inputAck(CHANNEL_ID, 9, 1);
    expect(await wrote).toEqual({ status: "accepted", writtenBytes: 1 });
  });

  test("a pre-mutation failure rejects and leaves no claim; a post-mutation failure is ambiguous", async () => {
    const f = await fixture({ cols: 80, rows: 24, seed: "x" });
    const first = f.mgr.applyTerminalViewport(claim({ clientSeq: 4n, cols: 100, rows: 30 }));
    await settle();
    f.keeper.resizeAck(CHANNEL_ID, 1, 100, 30);
    expect((await first).status).toBe("committed");

    // Pre-mutation: a stale sequence is validated before anything mutates.
    const stale = await f.mgr.applyTerminalViewport(claim({ clientSeq: 3n, cols: 10, rows: 10 }));
    expect(stale).toEqual({ status: "rejected", reason: "stale viewport sequence" });
    expect(f.mgr.viewportClaims.get(CHANNEL_ID)!.get(VIEWER)).toMatchObject({ cols: 100, rows: 30 });
    expect(f.keeper.seqOf(MuxFrameType.ResizeRequest)).toEqual([1]);

    // Post-mutation: the claim is installed and the resize admitted, then the
    // rebuild fails. The PTY may already be resized, so this can only be
    // ambiguous — never a fabricated rejection.
    const rec = f.mgr.sessions.get(CHANNEL_ID)!;
    const applied = f.mgr.applyTerminalViewport(claim({ clientSeq: 5n, cols: 120, rows: 40 }));
    await settle();
    Object.defineProperty(rec, "scrollback", {
      get() { throw new Error("injected post-mutation failure"); },
      configurable: true,
    });
    f.keeper.resizeAck(CHANNEL_ID, 2, 120, 40);
    const result = await applied;

    expect(result.status).toBe("ambiguous");
    expect(result).toMatchObject({ reason: expect.stringContaining("injected post-mutation failure") });
    // The claim the caller installed survives: nothing proved it wrong.
    expect(f.mgr.viewportClaims.get(CHANNEL_ID)!.get(VIEWER)).toMatchObject({ cols: 120, rows: 40 });
    expect(f.mgr.cellEmissionGates.has(CHANNEL_ID)).toBe(false);
    expect(f.mgr.resizeCaptures.has(CHANNEL_ID)).toBe(false);
  });

  test("a freshness reap and a typed claim cannot build two cores for one decision", async () => {
    const f = await fixture({ cols: 80, rows: 24, seed: "z" });
    // A stale claim that the reaper will exclude from the SCD, plus the live
    // claim whose size both owners must converge on.
    f.mgr.viewportClaims.set(CHANNEL_ID, new Map([
      ["viewer:dead", { cols: 60, rows: 20, lastMs: Date.now() - 90_000, clientSeq: 1n }],
      [VIEWER, { cols: 100, rows: 30, lastMs: Date.now(), clientSeq: 1n }],
    ]));

    f.mgr._reapViewportClaims();
    const applied = f.mgr.applyTerminalViewport(claim({ clientSeq: 2n, cols: 100, rows: 30 }));
    await settle();

    // Serialized on one owner: the reap's resize is the only one in flight.
    expect(f.keeper.seqOf(MuxFrameType.ResizeRequest)).toEqual([1]);
    f.keeper.resizeAck(CHANNEL_ID, 1, 100, 30);
    const result = await applied;
    await settle();

    expect(result.status).toBe("committed");
    // The claim ran after the reap, saw the proven size, and installed no second
    // capture — one decision, one core.
    expect(f.mgr.coreRebuilds.get(CHANNEL_ID)).toBe(1);
    expect(f.keeper.seqOf(MuxFrameType.ResizeRequest)).toEqual([1]);
    expect(f.mgr.lastAppliedSize.get(CHANNEL_ID)).toEqual({ cols: 100, rows: 30 });
  });

  test("an alt-screen boundary alt-primes the boundary core and applies only the tail", async () => {
    // Alt-screen history is absolute cursor moves painted for the OLD width;
    // replaying it at a new width duplicates and mangles rows. The recorded
    // boundary mode — not the mode at rebuild time — decides that, which is only
    // correct because the boundary is stamped inside the result frame.
    const f = await fixture({ cols: 80, rows: 24, seed: "\x1b[?1049hTUI-OLD\r\n" });
    f.mgr.sessions.get(CHANNEL_ID)!.alt_mode = true;
    const applied = f.mgr.applyTerminalViewport(claim({ clientSeq: 1n, cols: 100, rows: 30 }));
    await settle();

    f.keeper.resizeAck(CHANNEL_ID, 1, 100, 30);
    // The TUI's post-SIGWINCH repaint is the whole truth at the new geometry.
    ptyOut(f.mgr, "TUI-REPAINT");
    const result = await applied;

    expect(result).toMatchObject({ status: "committed", cols: 100, rows: 30 });
    expect(f.mgr.coreRebuilds.get(CHANNEL_ID)).toBe(1);
    // Pre-boundary bytes were NOT replayed; the post-boundary tail was, once.
    expect(coreText(f.mgr)).toBe("TUI-REPAINT");
    const core = f.mgr.sessions.get(CHANNEL_ID)!.wtermCore;
    expect(core.usingAltScreen()).toBe(true);
    // Retained history is never rewritten by a rebuild decision.
    expect(retained(f.mgr)).toBe("\x1b[?1049hTUI-OLD\r\nTUI-REPAINT");
  });

  test("a resize inside an open synchronized frame retires the hold with the core", async () => {
    // A SIGWINCH landing inside an unclosed mode-2026 frame is two governors
    // meeting: the emitter's synchronized-output hold, whose documented recovery
    // ceiling is one second, and this transaction's cell-emission gate, whose
    // ceiling is its phase budget. emitCellFrame checks the gate BEFORE the force
    // bypass, so a hold left armed across the transaction trips into the gate and
    // has its frame discarded — the second is spent invisibly, and the chunk
    // after the gate clears then opens a FRESH second on top. The hold belongs to
    // one core instance, so the transaction retires it.
    const f = await fixture({ cols: 80, rows: 24, seed: "before\r\n" });
    // A watched channel at the size already proven: installs the claim the cell
    // governor needs without a resize.
    expect(await f.mgr.applyTerminalViewport(claim({ clientSeq: 1n, cols: 80, rows: 24 })))
      .toMatchObject({ status: "committed", resized: false });
    ptyOut(f.mgr, `${SYNC_ON}mid repaint\r\n`);
    const stale = f.mgr.syncOutputHolds.get(CHANNEL_ID);
    expect(stale).toBeDefined();
    const publishedBefore = f.cells.length;

    const applied = f.mgr.applyTerminalViewport(claim({ clientSeq: 2n, cols: 100, rows: 30 }));
    await settle();
    // Retired in the same synchronous step that installed the gate: its
    // generation and its row ceiling were the now-frozen core's numbers.
    expect(f.mgr.cellEmissionGates.has(CHANNEL_ID)).toBe(true);
    expect(f.mgr.syncOutputHolds.has(CHANNEL_ID)).toBe(false);
    expect(f.cells).toHaveLength(publishedBefore);

    // Output keeps arriving inside the frame the application never closed.
    ptyOut(f.mgr, "during\r\n");
    f.keeper.resizeAck(CHANNEL_ID, 1, 100, 30);
    ptyOut(f.mgr, "after\r\n");
    const result = await applied;

    expect(result).toMatchObject({ status: "committed", cols: 100, rows: 30 });
    expect(f.mgr.coreRebuilds.get(CHANNEL_ID)).toBe(1);
    expect(f.mgr.cellEmissionGates.has(CHANNEL_ID)).toBe(false);
    // The replacement core still carries the unclosed opener. Its required
    // authoritative snapshot waits on a NEW hold against that core rather than
    // publishing replayed intermediate cells.
    expect(f.cells).toHaveLength(publishedBefore);
    expect(f.mgr.pendingSyncCellSnapshots.has(CHANNEL_ID)).toBe(true);
    const core = f.mgr.shellByChannel(CHANNEL_ID)!.wtermCore;
    expect(core.synchronizedOutput?.()).toBe(true);
    const fresh = f.mgr.syncOutputHolds.get(CHANNEL_ID);
    expect(fresh).toBeDefined();
    expect(fresh).not.toBe(stale);
    expect(fresh!.generation).toBe(core.synchronizedOutputGeneration?.() ?? 0);

    // The application boundary releases exactly one complete full frame at the
    // replacement geometry.
    ptyOut(f.mgr, `post-resize${SYNC_OFF}`);
    expect(f.mgr.syncOutputHolds.has(CHANNEL_ID)).toBe(false);
    expect(f.mgr.pendingSyncCellSnapshots.has(CHANNEL_ID)).toBe(false);
    expect(f.cells).toHaveLength(publishedBefore + 1);
    const published = f.cells.at(-1)!;
    expect(published.full).toBe(true);
    expect([published.cols, published.rows]).toEqual([100, 30]);
  });
});
