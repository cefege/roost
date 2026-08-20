// DEC private mode 2026 (synchronized output) frame suppression, and the two
// ceilings that bound it.
//
// An application that opens a synchronized frame is telling the renderer not to
// paint a half-drawn grid, so the emitter withholds streaming cell frames until
// the frame closes. That is correct right up until the frame never closes — a
// TUI killed mid-repaint, a truncated recording, a `printf` that emitted only
// the opener — at which point an unbounded hold is indistinguishable from the
// terminal-stream stall this whole plan section exists to remove: the browser
// goes dark while the core keeps parsing, forever, with nothing in the logs.
//
// Two ceilings, because the two stuck shapes are different. A stuck stream that
// goes SILENT produces no further chunks, so only an armed timer can recover it.
// A stuck stream that keeps FLOODING is caught by accumulated work long before
// the wall clock. Both force the withheld frame out and stop that generation
// suppressing anything further, and both stay legible in the diagnostic
// snapshot until the application finally closes its frame.

import { describe, test, expect, jest } from "bun:test";
import { WasmBridge } from "@wterm/core";
import { asChannelId, asSessionId, asWorkerFp } from "@roost/shared/wire";
import { initCellEmitState, scrollbackOrigin, type CellGridFrame, type CellRow } from "@roost/shared/cell";
import { protoToCellFrame } from "@roost/shared/cell/cell-proto";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import { SessionManager } from "../src/session-manager.ts";
import {
  CELL_EMIT_COALESCE_MS,
  SYNC_OUTPUT_MAX_MS,
  SYNC_OUTPUT_MAX_PENDING_ROWS,
} from "../src/session-constants.ts";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import type { SessionShellRecord } from "../src/session-record.ts";
import {
  clearResizeCapture,
  installResizeCapture,
  rebuildTerminalCore,
} from "../src/session-resize-capture.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";

const CID = 1;
const SYNC_ON = "\x1b[?2026h";
const SYNC_OFF = "\x1b[?2026l";

interface Harness {
  mgr: SessionManager;
  frames: CellGridFrame[];
}

async function harness(): Promise<Harness> {
  const frames: CellGridFrame[] = [];
  const mgr = new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
    sendBinaryUpstream: () => {},
    sendCellGridUpstream: (_ch, frame: PbCellGridFrame) => { frames.push(protoToCellFrame(frame)); },
  });
  const wtermCore = await WasmBridge.load();
  wtermCore.init(80, 24);
  mgr.sessions.set(CID, {
    sessionId: asSessionId("00000000-0000-0000-0000-000000000000"),
    channelId: asChannelId(CID),
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
    query_carry: new Uint8Array(0),
    ...initAgentOscState(),
    wtermCore,
    session_trace_id: "sync0000",
    cell_emit: initCellEmitState("test-grid"),
    sb_origin_pin: null,
    lastPtyOutMs: 0,
  } as never);
  // Floor seed (every real spawn records it) + a watcher, since cells only flow to a watched channel.
  mgr.channelResizeSeq.set(CID, 0);
  mgr.claimViewport(CID, "viewer", 80, 24, 1, 1);
  frames.length = 0; // the claim's own snapshot is not part of what the ceilings govern
  return { mgr, frames };
}

/** The real PTY entrypoint: ring append, core write, then the cell governor. */
function chunk(mgr: SessionManager, text: string): void {
  mgr.emitUpstreamChunk(CID, Buffer.from(text, "binary"));
}

function lines(from: number, count: number): string {
  let out = "";
  for (let i = from; i < from + count; i++) out += `SYNCLINE-${i}\r\n`;
  return out;
}

function textOf(row: CellRow): string {
  return row.spans.map((span) => span.text).join("").trimEnd();
}

function rec(mgr: SessionManager): SessionShellRecord {
  const record = mgr.shellByChannel(CID);
  if (record === undefined) throw new Error("no session record for the channel");
  return record;
}

interface GateReport {
  active: boolean;
  gate: string | null;
  age_ms: number | null;
  suppressed_frames: number;
  over_budget: boolean;
  budget_ms: number;
}

function gateOf(mgr: SessionManager): GateReport {
  const snapshot = mgr.diagSnapshot() as {
    sessions: Record<string, { gate: GateReport }>;
  };
  const session = Object.values(snapshot.sessions)[0];
  if (session === undefined) throw new Error("no session in the diagnostic snapshot");
  return session.gate;
}

function teardown(mgr: SessionManager): void {
  mgr._disposeOutputState(CID);
  mgr.dispose();
}

describe("synchronized output withholds intermediate frames", () => {
  test("an open frame suppresses every streaming send and says so", async () => {
    const { mgr, frames } = await harness();
    chunk(mgr, SYNC_ON);
    for (let i = 0; i < 5; i++) chunk(mgr, `redraw-${i}\r\n`);

    expect(frames).toHaveLength(0);
    const gate = gateOf(mgr);
    expect(gate).toMatchObject({
      active: true,
      gate: "sync_output",
      suppressed_frames: 6,
      over_budget: false,
      budget_ms: SYNC_OUTPUT_MAX_MS,
    });
    teardown(mgr);
  });

  test("a trailing coalesce timer armed before the frame cannot fire through it", async () => {
    // The leak that makes suppression cosmetic: an ordinary streaming session
    // has a 16 ms trailing timer armed and re-arming. If it survives the frame
    // opening, the browser is handed a half-drawn grid every window and the hold
    // has bought nothing.
    const { mgr, frames } = await harness();
    jest.useFakeTimers();
    try {
      chunk(mgr, "streaming\r\n");
      await Promise.resolve();
      expect(frames).toHaveLength(1); // leading edge, then the trailing arm
      frames.length = 0;

      chunk(mgr, `${SYNC_ON}half a repaint\r\n`);
      jest.advanceTimersByTime(CELL_EMIT_COALESCE_MS * 4);
      expect(frames).toHaveLength(0);
      teardown(mgr);
    } finally {
      jest.useRealTimers();
    }
  });

  test("a leading microtask queued before the frame cannot fire through it", async () => {
    // The other deferred path, and the one the scheduler cannot cancel: the
    // microtask is already queued, so only the emit itself can hold it.
    const { mgr, frames } = await harness();
    chunk(mgr, "streaming\r\n");      // queues the leading-edge microtask
    chunk(mgr, `${SYNC_ON}half a repaint\r\n`); // opens the frame first
    await Promise.resolve();
    await Promise.resolve();
    expect(frames).toHaveLength(0);
    expect(mgr.syncOutputHolds.has(CID)).toBe(true);
    // And the withheld state is not lost — the close ships all of it at once, as
    // a DELTA: suppression never advanced rec.cell_emit or cleared a dirty bit,
    // so the emitter's own reframe test still describes the frame the browser is
    // holding and every withheld row is still dirty.
    chunk(mgr, `done${SYNC_OFF}`);
    expect(frames).toHaveLength(1);
    expect(frames[0]!.full).toBe(false);
    const painted = frames[0]!.viewportRows
      .flatMap((r) => r.spans.map((s) => s.text)).join("\n");
    expect(painted).toContain("streaming");
    expect(painted).toContain("half a repaint");
    expect(painted).toContain("done");
    teardown(mgr);
  });

  test("a returning viewer's full snapshot waits for the synchronized paint boundary", async () => {
    const { mgr, frames } = await harness();
    mgr.viewportClaims.get(CID)?.clear();
    // With no viewer, output still advances the canonical core but does not
    // enter the streaming scheduler, so no hold record exists yet.
    chunk(mgr, `${SYNC_ON}\x1b[2J\x1b[1;1Hbefore-repair`);
    expect(mgr.syncOutputHolds.has(CID)).toBe(false);
    // TAB_VISIBLE attaches midway through the atomic paint. The claim must
    // discover mode 2026 from the core itself and defer its required full frame.
    mgr.claimViewport(CID, "viewer", 80, 24, 2, 3, 0);
    expect(frames).toHaveLength(0);
    expect(mgr.syncOutputHolds.get(CID)?.tripped).toBe(false);
    expect(mgr.pendingSyncCellSnapshots.has(CID)).toBe(true);
    chunk(mgr, `\x1b[2;1Hafter-repair${SYNC_OFF}`);
    expect(frames).toHaveLength(1);
    expect(frames[0]!.full).toBe(true);
    const painted = frames[0]!.viewportRows.map(textOf).join("\n");
    expect(painted).toContain("before-repair");
    expect(painted).toContain("after-repair");
    expect(mgr.pendingSyncCellSnapshots.has(CID)).toBe(false);
    expect(mgr.syncOutputHolds.has(CID)).toBe(false);
    teardown(mgr);
  });

  test("closing the frame ships the withheld state at that boundary, in one frame", async () => {
    const { mgr, frames } = await harness();
    chunk(mgr, SYNC_ON);
    chunk(mgr, "half a repaint\r\n");
    expect(frames).toHaveLength(0);

    chunk(mgr, `the rest${SYNC_OFF}`);
    // One frame, not one per withheld chunk — and a delta, not a forced full
    // grid. The application declared this instant a paint boundary; it did not
    // invalidate the browser's grid, and nothing about the suppressed window
    // makes the emitter's delta bookkeeping less true than it was.
    expect(frames).toHaveLength(1);
    expect(frames[0]!.full).toBe(false);
    expect(frames[0]!.viewportRows.some((r) => r.spans.some((s) => s.text.includes("the rest"))))
      .toBe(true);
    // The hold and its diagnostic record are released together.
    expect(mgr.syncOutputHolds.has(CID)).toBe(false);
    expect(gateOf(mgr)).toMatchObject({ active: false, gate: null, suppressed_frames: 0 });
    teardown(mgr);
  });

  test("a frame opened and closed inside one chunk never suppresses at all", async () => {
    const { mgr, frames } = await harness();
    chunk(mgr, `${SYNC_ON}a complete repaint${SYNC_OFF}`);
    expect(mgr.syncOutputHolds.has(CID)).toBe(false);
    await Promise.resolve();
    expect(frames).toHaveLength(1);
    // A DELTA off the ordinary leading edge, not a forced full frame. Modern
    // TUIs wrap EVERY 60fps repaint in mode 2026 and almost always land the
    // whole block in one PTY read; charging them a full grid per frame would
    // cost 24 rows where one changed.
    expect(frames[0]!.full).toBe(false);
    teardown(mgr);
  });

  test("the closed frame's delta carries the history that scrolled past inside it", async () => {
    // What the forced full frame cost twice over: gridToCellFrame reads every
    // viewport cell unconditionally AND carries zero history rows, so lines that
    // left the viewport during the suppressed window were simply absent from the
    // frame and came back later as a separate backfill round trip. The delta's
    // append range is [lastSbTotal, current), and suppression never moved
    // lastSbTotal, so those lines are already inside it.
    const { mgr, frames } = await harness();
    chunk(mgr, "anchor\r\n");
    await Promise.resolve();
    expect(frames).toHaveLength(1);
    const heldTotal = frames[0]!.scrollbackTotal;
    frames.length = 0;

    // 40 newline-terminated lines into a 24-row grid: 17 of them plus the anchor
    // leave the viewport for history while the frame is open.
    chunk(mgr, SYNC_ON);
    chunk(mgr, lines(1, 40));
    expect(frames).toHaveLength(0);
    chunk(mgr, SYNC_OFF);

    expect(frames).toHaveLength(1);
    const frame = frames[0]!;
    expect(frame.full).toBe(false);
    expect(frame.scrollbackTotal).toBeGreaterThan(heldTotal);
    // Contiguous from exactly where the browser was: no hole for the backfill
    // controller to discover, and every index names the line it claims.
    const appended = frame.scrollbackAppend;
    expect(appended.map((row) => row.index))
      .toEqual(Array.from({ length: frame.scrollbackTotal - heldTotal }, (_, i) => heldTotal + i));
    for (const row of appended) {
      expect(textOf(row)).toBe(row.index === 0 ? "anchor" : `SYNCLINE-${row.index}`);
    }
    // And the viewport half of the burst is in the same frame.
    expect(frame.viewportRows.map(textOf)).toContain("SYNCLINE-40");
    teardown(mgr);
  });
});

describe("the suppression is bounded", () => {
  test("the wall ceiling recovers a stuck frame whose producer went silent", async () => {
    const { mgr, frames } = await harness();
    jest.useFakeTimers();
    try {
      chunk(mgr, `${SYNC_ON}drawing...\r\n`);
      // The pathological stream: mode 2026 opened, output written, the closer
      // never sent, and then nothing at all. No later chunk exists to
      // re-evaluate the hold, so without the armed timer this channel is dark
      // for as long as the session lives.
      expect(frames).toHaveLength(0);
      jest.advanceTimersByTime(SYNC_OUTPUT_MAX_MS - 1);
      expect(frames).toHaveLength(0);

      jest.advanceTimersByTime(1);
      expect(frames).toHaveLength(1);
      expect(frames[0]!.full).toBe(false);
      expect(frames[0]!.viewportRows.some((r) => r.spans.some((s) => s.text.includes("drawing"))))
        .toBe(true);

      // Still inside a frame the application never closed — the trip is not the
      // end of the fault, so the snapshot keeps reporting it.
      const hold = mgr.syncOutputHolds.get(CID);
      expect(hold?.tripped).toBe(true);
      expect(gateOf(mgr)).toMatchObject({
        active: true,
        gate: "sync_output",
        over_budget: true,
        suppressed_frames: 1,
        budget_ms: SYNC_OUTPUT_MAX_MS,
      });
      teardown(mgr);
    } finally {
      jest.useRealTimers();
    }
  });

  test("the work ceiling recovers a stuck frame that keeps flooding", async () => {
    const { mgr, frames } = await harness();
    chunk(mgr, SYNC_ON);
    let written = 0;
    // 100 rows per chunk: well under the ceiling, so only the accumulated total
    // can trip it. No timer is involved — this must resolve on work alone.
    while (frames.length === 0 && written < SYNC_OUTPUT_MAX_PENDING_ROWS * 3) {
      chunk(mgr, lines(written + 1, 100));
      written += 100;
    }
    expect(written).toBeGreaterThanOrEqual(SYNC_OUTPUT_MAX_PENDING_ROWS);
    // Bounded from above too: the browser is never more than one chunk past the
    // ceiling behind the core.
    expect(written).toBeLessThanOrEqual(SYNC_OUTPUT_MAX_PENDING_ROWS + 100);
    expect(frames).toHaveLength(1);
    // FULL here, and not because the trip forced it: 2,100 lines overran the
    // stock core's 1,000-line ring, so the eviction origin passed the browser's
    // watermark and a delta's append would splice a hole. nextCellFrame's own
    // `sbDropped > lastSbTotal` reframe test is what decides that — the exact
    // case where the withheld history genuinely cannot be delivered inline.
    expect(rec(mgr).cell_emit.sbDropped).toBeGreaterThan(0);
    expect(frames[0]!.full).toBe(true);
    // A semantic reframe, so the epoch advances and every absolute index the
    // browser held is explicitly retired rather than silently re-aliased.
    expect(frames[0]!.gridEpoch).toBe("test-grid:1");
    expect(mgr.syncOutputHolds.get(CID)?.tripped).toBe(true);
    expect(gateOf(mgr)).toMatchObject({ gate: "sync_output", over_budget: true });
    teardown(mgr);
  });

  test("a tripped generation stops suppressing, and the next one starts fresh", async () => {
    const { mgr, frames } = await harness();
    jest.useFakeTimers();
    try {
      chunk(mgr, `${SYNC_ON}stuck\r\n`);
      jest.advanceTimersByTime(SYNC_OUTPUT_MAX_MS);
      expect(frames).toHaveLength(1);

      // Bypassed for the rest of this stuck generation: output flows on the
      // ordinary governor even though mode 2026 is still set.
      frames.length = 0;
      chunk(mgr, "still stuck but visible\r\n");
      await Promise.resolve();
      expect(frames).toHaveLength(1);
      expect(frames[0]!.full).toBe(false);

      // A genuinely new frame gets the full ceiling again — the bypass is scoped
      // to the generation that broke it, not to the channel.
      frames.length = 0;
      chunk(mgr, `${SYNC_OFF}${SYNC_ON}next frame\r\n`);
      expect(frames).toHaveLength(0);
      const hold = mgr.syncOutputHolds.get(CID);
      expect(hold?.tripped).toBe(false);
      expect(hold?.generation).toBe(2);
      teardown(mgr);
    } finally {
      jest.useRealTimers();
    }
  });

  test("closing and reopening every chunk cannot keep resetting the ceiling", async () => {
    const { mgr, frames } = await harness();
    jest.useFakeTimers();
    try {
      // The subtler pathological stream: each chunk closes the previous frame
      // and immediately opens the next, so every chunk observes a NEW
      // synchronized-output generation and the browser never sees a boundary.
      // Restarting the ceilings per generation would let this suppress forever,
      // one reset at a time.
      chunk(mgr, `${SYNC_ON}frame 0\r\n`);
      const framesAtTick: number[] = [];
      // One tenth of the ceiling per iteration, so the ORIGINAL deadline lands
      // on tick 10 and a per-generation restart would push it out of reach on
      // every single tick.
      for (let tick = 1; tick <= 20; tick++) {
        jest.advanceTimersByTime(SYNC_OUTPUT_MAX_MS / 10);
        framesAtTick.push(frames.length);
        chunk(mgr, `${SYNC_OFF}${SYNC_ON}frame ${tick}\r\n`);
      }
      expect(mgr.syncOutputHolds.get(CID)?.generation).toBe(21);
      // Twenty generations later, the deadline the FIRST hold armed is what
      // fired — on tick 10, not tick 20 and not never.
      expect(framesAtTick.indexOf(1)).toBe(9);
      expect(frames[0]!.full).toBe(false);
      // And it keeps recovering: the trip retires that hold, tick 10's chunk
      // opens a fresh one, and its ceiling fires on tick 20. A permanently
      // toggling producer is throttled to one frame per ceiling, never zero.
      expect(frames).toHaveLength(2);
      teardown(mgr);
    } finally {
      jest.useRealTimers();
    }
  });

  test("teardown disarms the hold instead of leaving a timer on a dead channel", async () => {
    const { mgr, frames } = await harness();
    jest.useFakeTimers();
    try {
      chunk(mgr, `${SYNC_ON}orphan\r\n`);
      expect(mgr.syncOutputHolds.has(CID)).toBe(true);

      mgr._disposeOutputState(CID);
      expect(mgr.syncOutputHolds.has(CID)).toBe(false);
      expect(mgr.cellGateSuppression.has(CID)).toBe(false);

      jest.advanceTimersByTime(SYNC_OUTPUT_MAX_MS * 2);
      expect(frames).toHaveLength(0);
      mgr.dispose();
    } finally {
      jest.useRealTimers();
    }
  });

  test("a resize inside an open frame retires the hold instead of arming it at a dead core", async () => {
    // The interaction the ceilings were never reconciled against. A SIGWINCH
    // landing inside an unclosed synchronized frame installs a cell-emission
    // gate that emitCellFrame checks BEFORE the force bypass, so the hold's own
    // wall timer would trip, signal, build a full frame from the frozen core and
    // have it discarded — spending the documented 1 s ceiling invisibly, and
    // leaving the next chunk to open a FRESH 1 s ceiling on top of the
    // transaction's own multi-second budget.
    const { mgr, frames } = await harness();
    jest.useFakeTimers();
    try {
      chunk(mgr, `${SYNC_ON}mid repaint\r\n`);
      const stale = mgr.syncOutputHolds.get(CID);
      expect(stale).toBeDefined();

      // The transaction's three real steps, in order, with no keeper in the way.
      const capture = installResizeCapture(mgr, CID, "viewport_resize");
      // Retired WITH the gate, in the same synchronous step: its generation and
      // its row ceiling are the frozen core's numbers.
      expect(mgr.syncOutputHolds.has(CID)).toBe(false);
      expect(mgr.cellEmissionGates.has(CID)).toBe(true);

      // Output keeps arriving at an unproven geometry and the clock runs three
      // ceilings past where the stale hold had armed. Nothing fires.
      chunk(mgr, "during the resize\r\n");
      jest.advanceTimersByTime(SYNC_OUTPUT_MAX_MS * 3);
      expect(frames).toHaveLength(0);
      expect(mgr.syncOutputHolds.has(CID)).toBe(false);

      expect(await rebuildTerminalCore(mgr, CID, 100, 24, capture)).toBe(true);
      clearResizeCapture(mgr, CID);
      mgr.emitCellSnapshot(asChannelId(CID));

      // The rebuilt core still holds the unclosed 2026h. Its authoritative
      // snapshot therefore opens a fresh hold on the replacement core instead
      // of publishing the replay's intermediate grid.
      expect(frames).toHaveLength(0);
      expect(mgr.pendingSyncCellSnapshots.has(CID)).toBe(true);
      const core = rec(mgr).wtermCore;
      const fresh = mgr.syncOutputHolds.get(CID);
      expect(fresh).toBeDefined();
      expect(fresh).not.toBe(stale);
      // `?? 0` mirrors syncOutputAction's own read: the generation is optional
      // on the TerminalCore interface, and 0 is what a core without it reports.
      expect(fresh!.generation).toBe(core.synchronizedOutputGeneration?.() ?? 0);
      // Measured against the REBUILT core's numbering, not the retired one's.
      expect(fresh!.sbTotalAtOpen)
        .toBe(scrollbackOrigin(core, rec(mgr).cell_emit) + core.getScrollbackCount());
      // The normal synchronized-output ceiling remains the bounded fallback for
      // an application that never declares the paint complete.
      jest.advanceTimersByTime(SYNC_OUTPUT_MAX_MS - 1);
      expect(frames).toHaveLength(0);
      jest.advanceTimersByTime(1);
      expect(frames).toHaveLength(1);
      expect(frames[0]!.full).toBe(true);
      expect(frames[0]!.cols).toBe(100);
      expect(mgr.pendingSyncCellSnapshots.has(CID)).toBe(false);
      teardown(mgr);
    } finally {
      jest.useRealTimers();
    }
  });
});
