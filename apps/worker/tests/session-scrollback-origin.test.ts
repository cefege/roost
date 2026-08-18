// Where the ring's eviction origin comes from, and what breaks when it is
// stale.
//
// Every absolute row index Roost hands a browser is measured from `sbDropped` —
// the lines the ring has discarded. Get that origin wrong by N and the worker
// still returns real, well-formed rows, just the WRONG ones: the reader lands N
// lines away, a find hit points at the wrong text, and a backfill page splices
// history that never sat there. Nothing throws, so it can only be caught by
// asserting the identity of the rows themselves.
//
// 0.3.4 counts discards in the core (getScrollbackDiscardedCount), so the
// emitter reads the origin instead of inferring it, and — the part these tests
// exist for — the read paths take it LIVE from the core rather than from the
// last emitted frame. The ring keeps evicting between emits; a browser's
// backfill range must never disagree with it.

import { describe, expect, test } from "bun:test";
import { WasmBridge } from "@wterm/core";
import { asChannelId, asSessionId, asWorkerFp } from "@roost/shared";
import {
  initCellEmitState,
  readScrollbackRangeCells,
  scrollbackOrigin,
  type CellGridFrame,
  type CellRow,
} from "@roost/shared/cell";
import { protoToCellFrame } from "@roost/shared/cell/cell-proto";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import type { ClientControlFrame } from "@roost/shared/wire";
import { handleGetScrollbackCells } from "../src/browser-command-terminal.ts";
import { rebuildTerminalCore } from "../src/session-resize-capture.ts";
import { SessionManager } from "../src/session-manager.ts";
import type { SessionShellRecord } from "../src/session-record.ts";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";
import type { CoordLink } from "../src/transport/CoordLink.ts";

const SID = asSessionId("00000000-0000-0000-0000-0000000000aa");
const CID = 7;
const COLS = 80;
const ROWS = 24;
// The stock inline core's ring. Deep enough to be a real rollover, shallow
// enough that saturating it costs milliseconds.
const CAPACITY = 1_000;
// Newline-terminated lines only reach history once they leave the viewport, so
// writing N of them into a fresh grid pushes N - (ROWS - 1); history index j
// therefore holds line j + 1, for every j, forever. That identity is the whole
// assertion: it is what an absolute index MEANS.
const pushedBy = (lines: number): number => lines - (ROWS - 1);
const SYNC_ON = "\x1b[?2026h";

interface Harness {
  mgr: SessionManager;
  rec: SessionShellRecord;
  frames: CellGridFrame[];
}

/** `ringBytes` shrinks the RETAINED BYTE window, which is what bounds a rebuild
 *  replay. A small ring makes the rebuild reproduce far fewer lines than the old
 *  core's monotonic total, so the origin pin's margin is genuinely positive
 *  rather than absorbed by its own Math.max(0, …) floor. */
async function harness(ringBytes?: number): Promise<Harness> {
  const frames: CellGridFrame[] = [];
  const mgr = new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
    sendBinaryUpstream: () => {},
    sendCellGridUpstream: (_ch, frame: PbCellGridFrame) => { frames.push(protoToCellFrame(frame)); },
  });
  const wtermCore = await WasmBridge.load();
  wtermCore.init(COLS, ROWS);
  const rec = {
    sessionId: SID,
    channelId: asChannelId(CID),
    socketPath: "/dev/null",
    kind: "shell" as const,
    cwd: "/",
    fsm: {} as never,
    bridge: null,
    scrollback: createSbRing(undefined, ringBytes),
    head_seq: 0,
    alt_mode: false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    query_carry: new Uint8Array(0),
    ...initAgentOscState(),
    wtermCore,
    session_trace_id: "sborigin",
    cell_emit: initCellEmitState("test-grid"),
    sb_origin_pin: null,
    lastPtyOutMs: 0,
  } as unknown as SessionShellRecord;
  mgr.sessions.set(CID, rec);
  return { mgr, rec, frames };
}

/** The real PTY entrypoint. No viewer is claiming the channel, so this advances
 *  the ring and the core without scheduling any cell emit — the test decides
 *  exactly when a frame is produced, and therefore how stale a last-emit origin
 *  would be. */
function feed(mgr: SessionManager, from: number, count: number): void {
  let batch = "";
  for (let i = from; i < from + count; i++) {
    batch += `ORIGINLINE-${i}\r\n`;
    if ((i - from + 1) % 250 === 0) {
      mgr.emitUpstreamChunk(CID, Buffer.from(batch, "binary"));
      batch = "";
    }
  }
  if (batch.length > 0) mgr.emitUpstreamChunk(CID, Buffer.from(batch, "binary"));
}

function markerOf(row: CellRow): number {
  const text = row.spans.map((span) => span.text).join("");
  const hit = /ORIGINLINE-(\d+)/.exec(text);
  if (hit === null) throw new Error(`row ${row.index} is not a marked line: ${JSON.stringify(text)}`);
  return Number(hit[1]);
}

type RpcOk = {
  kind: "rpc-ok";
  request_id: string;
  data: { rows: CellRow[]; cols: number; total: number; start_row: number; end_row: number };
};
type RpcError = { kind: "rpc-error"; request_id: string; message: string };

function linkCapture(): { coordLink: CoordLink; sent: Array<RpcOk | RpcError> } {
  const sent: Array<RpcOk | RpcError> = [];
  const coordLink = { send: (frame: RpcOk | RpcError) => { sent.push(frame); } } as unknown as CoordLink;
  return { coordLink, sent };
}

async function backfill(mgr: SessionManager, endRow: number, maxRows: number): Promise<RpcOk["data"]> {
  const { coordLink, sent } = linkCapture();
  const frame: Extract<ClientControlFrame, { kind: "get-scrollback-cells" }> = {
    kind: "get-scrollback-cells",
    request_id: "req",
    session_id: SID,
    end_row: endRow,
    max_rows: maxRows,
    grid_epoch: "",
  };
  await handleGetScrollbackCells(frame, "req", { coordLink, sessionMgr: mgr });
  const reply = sent[0];
  if (reply === undefined || reply.kind !== "rpc-ok") {
    throw new Error(`backfill failed: ${JSON.stringify(reply)}`);
  }
  return reply.data;
}

describe("the emitted frame's history window comes from the core's counters", () => {
  test("sbBase and scrollbackTotal are the core's discarded count plus its retained count", async () => {
    const { mgr, rec, frames } = await harness();
    feed(mgr, 1, 1_300);
    const core = rec.wtermCore;
    const discarded = scrollbackOrigin(core, initCellEmitState("probe"));
    // A real rollover, not merely a full ring.
    expect(core.getScrollbackCount()).toBe(CAPACITY);
    expect(discarded).toBe(pushedBy(1_300) - CAPACITY);

    mgr.emitCellSnapshot(asChannelId(CID));
    const frame = frames.at(-1)!;
    expect(frame.full).toBe(true);
    expect(frame.scrollbackTotal).toBe(discarded + CAPACITY);
    // Authoritative frames are viewport-only, so the base sits at the total and
    // the retained depth stays addressable through the backfill alone.
    expect(frame.sbBase).toBe(frame.scrollbackTotal);
    expect(rec.cell_emit.sbDropped).toBe(discarded);
    mgr._disposeOutputState(CID);
    mgr.dispose();
  });

  test("a backfill page names the lines its indices claim, across the rollover", async () => {
    const { mgr, rec } = await harness();
    feed(mgr, 1, 1_300);
    mgr.emitCellSnapshot(asChannelId(CID));
    const total = rec.cell_emit.sbDropped + rec.wtermCore.getScrollbackCount();

    const page = await backfill(mgr, total, 200);
    expect(page.total).toBe(total);
    expect(page.rows).toHaveLength(200);
    for (const row of page.rows) expect(markerOf(row)).toBe(row.index + 1);
    mgr._disposeOutputState(CID);
    mgr.dispose();
  });

  test("history evicted between emits shifts the read, not the indices", async () => {
    // The regression this cutover closes. `rec.cell_emit.sbDropped` is only
    // written when a frame is EMITTED; the ring keeps rolling regardless. A read
    // path that trusts the last emitted origin resolves every absolute index
    // through offsets that are stale by exactly the lines discarded since.
    const { mgr, rec } = await harness();
    feed(mgr, 1, 1_300);
    mgr.emitCellSnapshot(asChannelId(CID));
    const staleOrigin = rec.cell_emit.sbDropped;

    const drift = 200;
    feed(mgr, 1_301, drift);
    const core = rec.wtermCore;
    const liveOrigin = scrollbackOrigin(core, rec.cell_emit);
    expect(liveOrigin).toBe(staleOrigin + drift);
    // No emit happened, so the frame-derived origin is now provably wrong.
    expect(rec.cell_emit.sbDropped).toBe(staleOrigin);

    const total = liveOrigin + core.getScrollbackCount();
    // Newest 50 rows: the page a reader at the bottom actually asks for.
    const tail = await backfill(mgr, total, 50);
    expect(tail.total).toBe(total);
    expect(tail.rows).toHaveLength(50);
    for (const row of tail.rows) expect(markerOf(row)).toBe(row.index + 1);

    // Now the counterfactual, so this test fails for the RIGHT reason if a read
    // path ever goes back to the last emitted origin. Probe just above the live
    // floor, where both the live and the stale window still cover the index.
    // Offsets run newest-first, so an origin that is `drift` too SMALL resolves
    // `drift` rows too NEW: the stale read succeeds and quietly returns the
    // wrong line. No error, no gap — just different history.
    const probe = liveOrigin + 10;
    const [live] = readScrollbackRangeCells(core, probe, probe + 1, liveOrigin);
    expect(markerOf(live!)).toBe(probe + 1);
    const [stale] = readScrollbackRangeCells(core, probe, probe + 1, staleOrigin);
    expect(markerOf(stale!)).toBe(probe + 1 + drift);
    // Further out, the stale window does not even reach the rows the ring holds,
    // so the same reader would be told history it can see does not exist.
    expect(readScrollbackRangeCells(core, total - 1, total, staleOrigin)).toEqual([]);
    mgr._disposeOutputState(CID);
    mgr.dispose();
  });

  test("a page below the retained floor is short rather than wrong", async () => {
    const { mgr, rec } = await harness();
    feed(mgr, 1, 1_300);
    mgr.emitCellSnapshot(asChannelId(CID));
    const core = rec.wtermCore;
    const origin = scrollbackOrigin(core, rec.cell_emit);
    expect(origin).toBeGreaterThan(0);

    // Ask for 400 rows ending 100 past the floor: only the 100 surviving ones
    // exist, and they must be the 100 the ring actually holds.
    const page = await backfill(mgr, origin + 100, 400);
    expect(page.start_row).toBe(origin);
    expect(page.rows).toHaveLength(100);
    expect(markerOf(page.rows[0]!)).toBe(origin + 1);
    for (const row of page.rows) expect(markerOf(row)).toBe(row.index + 1);
    mgr._disposeOutputState(CID);
    mgr.dispose();
  });
});

describe("a core rebuild moves the origin's base, never the indices", () => {
  test("replaying the ring into a deeper core keeps every line's absolute index", async () => {
    // A resize rebuilds a fresh core from the raw byte ring. Its own discarded
    // counter restarts at 0 and the roost factory's ring is 10x deeper, so the
    // retained window both moves and GROWS — and through all of it the newest
    // line has to keep the index the browser already holds for it.
    const { mgr, rec } = await harness();
    feed(mgr, 1, 1_300);
    mgr.emitCellSnapshot(asChannelId(CID));
    const before = rec.cell_emit;
    const newestIndex = before.lastSbTotal - 1;
    const [newestBefore] = readScrollbackRangeCells(
      rec.wtermCore, newestIndex, newestIndex + 1, before.sbDropped,
    );
    expect(markerOf(newestBefore!)).toBe(newestIndex + 1);

    expect(await rebuildTerminalCore(mgr, CID, 100, ROWS, null)).toBe(true);

    const fresh = rec.wtermCore;
    const origin = scrollbackOrigin(fresh, rec.cell_emit);
    // The pin: origin + retained is exactly the total the browser last saw.
    expect(origin + fresh.getScrollbackCount()).toBe(before.lastSbTotal);
    expect(rec.cell_emit.sbDropped).toBe(origin);
    const [newestAfter] = readScrollbackRangeCells(fresh, newestIndex, newestIndex + 1, origin);
    expect(markerOf(newestAfter!)).toBe(newestIndex + 1);
    // The deeper ring recovers history the 1k core had already dropped, and it
    // arrives under the indices those lines always had.
    expect(fresh.getScrollbackCount()).toBeGreaterThan(CAPACITY);
    expect(origin).toBeLessThan(before.sbDropped);
    const [recovered] = readScrollbackRangeCells(fresh, origin, origin + 1, origin);
    expect(markerOf(recovered!)).toBe(origin + 1);
    mgr._disposeOutputState(CID);
    mgr.dispose();
  });

  test("a rebuild inside an open synchronized frame pins from the core, not the frozen emit", async () => {
    // The pin's proof sketch assumed rec.cell_emit.lastSbTotal equals the OLD
    // core's true monotonic total at the freeze instant. It does not. While a DEC
    // 2026 synchronized-output hold is open the emitter withholds every frame, so
    // lastSbTotal stays where the last successful emit left it while the core
    // keeps appending underneath — bounded only by the hold's own ceilings
    // (SYNC_OUTPUT_MAX_PENDING_ROWS rows / SYNC_OUTPUT_MAX_MS). Pin off the
    // frozen value and every absolute index the rebuild hands out is short by
    // exactly that gap: real, well-formed rows under the wrong numbers.
    //
    // The 4 KiB byte ring is what makes the gap OBSERVABLE instead of absorbed.
    // It bounds the replay, so the fresh core reproduces far fewer lines than the
    // old core's total and the pin's margin is genuinely positive — an
    // understated total lands as a wrong index rather than as the Math.max(0, …)
    // floor both values would share.
    const { mgr, rec, frames } = await harness(4_096);
    // A watched channel: the cell governor only runs for a live viewer, and the
    // hold is the governor's state.
    mgr.claimViewport(CID, "viewer", COLS, ROWS, 1, 1);
    feed(mgr, 1, 1_300);
    mgr.emitCellSnapshot(asChannelId(CID));
    const staleTotal = rec.cell_emit.lastSbTotal;
    expect(staleTotal).toBe(pushedBy(1_300));

    frames.length = 0;
    mgr.emitUpstreamChunk(CID, Buffer.from(SYNC_ON, "binary"));
    expect(mgr.syncOutputHolds.has(CID)).toBe(true);
    feed(mgr, 1_301, 300);
    // Withheld, so the emitted watermark is frozen 300 lines behind the core.
    expect(frames).toHaveLength(0);
    expect(rec.cell_emit.lastSbTotal).toBe(staleTotal);
    const liveTotal = scrollbackOrigin(rec.wtermCore, rec.cell_emit)
      + rec.wtermCore.getScrollbackCount();
    expect(liveTotal).toBe(pushedBy(1_600));
    expect(liveTotal - staleTotal).toBe(300);

    expect(await rebuildTerminalCore(mgr, CID, 100, ROWS, null)).toBe(true);

    // The hold belonged to the core the swap just retired.
    expect(mgr.syncOutputHolds.has(CID)).toBe(false);
    const fresh = rec.wtermCore;
    const origin = scrollbackOrigin(fresh, rec.cell_emit);
    // The pin, against the AUTHORITATIVE total. Pinning off staleTotal puts this
    // 300 lower, and every index measured from it with it.
    expect(origin + fresh.getScrollbackCount()).toBe(liveTotal);
    expect(rec.cell_emit.sbDropped).toBe(origin);
    // Positive margin: the clamp is not what produced this number.
    expect(origin).toBeGreaterThan(0);
    // The field the post-rebuild spread used to leave at 0 — exactly what the
    // authoritative frame that follows the rebuild would set, so a DROPPED
    // snapshot send cannot leave the watermark rewound to nothing.
    expect(rec.cell_emit.lastSbTotal).toBe(liveTotal);

    // The property all of it exists for: the newest retained line names its own
    // absolute index.
    const newest = liveTotal - 1;
    const [newestRow] = readScrollbackRangeCells(fresh, newest, newest + 1, origin);
    expect(markerOf(newestRow!)).toBe(newest + 1);
    // And the counterfactual, so this fails for the RIGHT reason if the pin ever
    // goes back to the frozen value: under that origin, index staleTotal-1 is
    // handed the line that is really liveTotal — 300 lines away, silently.
    const staleOrigin = staleTotal - fresh.getScrollbackCount();
    const [misread] = readScrollbackRangeCells(fresh, staleTotal - 1, staleTotal, staleOrigin);
    expect(markerOf(misread!)).toBe(liveTotal);
    mgr._disposeOutputState(CID);
    mgr.dispose();
  });
});
