// Is history truth OBSERVABLE, and is missing history HONEST about why?
//
// Three ranges have to agree and had no single place to compare them: the raw
// byte ring the worker retains, the line ring the core retains, and the numbering
// the last emitted frame told the browser. They are governed by unrelated units
// (bytes vs. reflow-dependent lines), so they diverge — and the divergence is
// only ever visible as history that quietly is not there any more.
//
// What these tests pin down:
//   - the diagnostic snapshot reports the CORE's live range, the RING's byte
//     bounds, and the last rebuild's origin pin, and the live core reading really
//     does diverge from the last-emitted one while a frame is withheld;
//   - a short get-scrollback-cells page names WHICH floor it hit, and the two
//     causes are distinguished: "evicted" (the core's line ring rolled — gone
//     forever) versus "resize_replay" (a resize rebuilt the grid from the fixed
//     byte ring, which could not reach as far back as the core it replaced);
//   - the alt-screen divergence: alt-screen repaint bytes share the primary
//     screen's byte ring, so a long TUI dwell evicts pre-alt primary scrollback
//     that a never-resized session still holds — measured against a same-feed
//     control whose ring is large enough to keep it.

import { describe, expect, test } from "bun:test";
import { WasmBridge } from "@wterm/core";
import { asChannelId, asSessionId, asWorkerFp } from "@roost/shared/wire";
import {
  initCellEmitState,
  readScrollbackRangeCells,
  scrollbackOrigin,
  type CellRow,
} from "@roost/shared/cell";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import type { ClientControlFrame, ScrollbackHistoryFloor } from "@roost/shared/wire";
import { handleGetScrollbackCells } from "../src/browser-command-terminal.ts";
import { rebuildTerminalCore } from "../src/session-resize-capture.ts";
import { SessionManager } from "../src/session-manager.ts";
import type { SbOriginPin, SessionShellRecord } from "../src/session-record.ts";
import { createSbRing, readRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";
import type { CoordLink } from "../src/transport/coord-link.ts";

const SID = asSessionId("00000000-0000-0000-0000-0000000000cc");
const CID = 11;
const COLS = 80;
const ROWS = 24;
// The stock inline core's line ring. The rebuild's fresh core is the roost-patched
// 10k one, which is why a rebuild can RECOVER history the 1k core had dropped —
// and why losing history to a rebuild takes a byte ring that is genuinely too
// small, not merely a shallower core.
const CORE_LINES = 1_000;
// Small enough that an ordinary alt-screen dwell laps it in milliseconds, which
// is exactly the production shape (1 MiB vs. a TUI repainting KBs per frame).
const SMALL_RING = 4_096;

// A newline-terminated line reaches history only once it leaves the viewport, so
// feeding N lines into a fresh grid pushes N - (ROWS - 1) of them.
const pushedBy = (lines: number): number => lines - (ROWS - 1);

interface Harness {
  mgr: SessionManager;
  rec: SessionShellRecord;
}

async function harness(capBytes?: number): Promise<Harness> {
  const mgr = new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
    sendBinaryUpstream: () => {},
    sendCellGridUpstream: (_ch: number, _frame: PbCellGridFrame) => {},
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
    scrollback: createSbRing(undefined, capBytes),
    head_seq: 0,
    alt_mode: false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    query_carry: new Uint8Array(0),
    ...initAgentOscState(),
    wtermCore,
    session_trace_id: "sbtruth",
    cell_emit: initCellEmitState("truth-grid"),
    lastPtyOutMs: 0,
    sb_origin_pin: null,
  } as unknown as SessionShellRecord;
  mgr.sessions.set(CID, rec);
  return { mgr, rec };
}

function teardown(mgr: SessionManager): void {
  mgr._disposeOutputState(CID);
  mgr.dispose();
}

/** The real PTY entrypoint: advances the ring AND the core, and schedules no cell
 *  emit (no viewer is claiming), so a test decides exactly when a frame is
 *  produced and therefore how stale the last-emitted numbering is. */
function feed(mgr: SessionManager, text: string): void {
  mgr.emitUpstreamChunk(CID, Buffer.from(text, "binary"));
}

function feedLines(mgr: SessionManager, prefix: string, from: number, count: number): void {
  let batch = "";
  for (let i = from; i < from + count; i++) {
    batch += `${prefix}-${i}\r\n`;
    if ((i - from + 1) % 250 === 0) {
      feed(mgr, batch);
      batch = "";
    }
  }
  if (batch.length > 0) feed(mgr, batch);
}

/** One full-screen TUI repaint: absolute cursor moves and text, never a newline,
 *  so it grows the BYTE ring without adding one line to the core's scrollback. */
function altFrame(generation: number): string {
  let out = "";
  for (let row = 1; row <= 20; row++) out += `\x1b[${row};1HALTPAINT ${generation} ${"x".repeat(60)}`;
  return out;
}

function rowText(row: CellRow): string {
  return row.spans.map((span) => span.text).join("");
}

function markerOf(row: CellRow, prefix: string): number {
  const hit = new RegExp(`${prefix}-(\\d+)`).exec(rowText(row));
  if (hit === null) throw new Error(`row ${row.index} is not a ${prefix} line: ${JSON.stringify(rowText(row))}`);
  return Number(hit[1]);
}

/** Rows in the core's scrollback whose text carries `prefix`. The measurement the
 *  alt-screen finding turns on: does this history still exist at all? */
function retainedMatching(rec: SessionShellRecord, prefix: string): number {
  const core = rec.wtermCore;
  const dropped = scrollbackOrigin(core, rec.cell_emit);
  const rows = readScrollbackRangeCells(core, dropped, dropped + core.getScrollbackCount(), dropped);
  return rows.filter((row) => rowText(row).includes(prefix)).length;
}

type CellsPage = {
  rows: CellRow[];
  cols: number;
  total: number;
  start_row: number;
  end_row: number;
  grid_epoch: string;
  history_floor: ScrollbackHistoryFloor;
};
type RpcOk = { kind: "rpc-ok"; request_id: string; data: CellsPage };
type RpcError = { kind: "rpc-error"; request_id: string; message: string };

async function backfill(mgr: SessionManager, endRow: number, maxRows: number): Promise<CellsPage> {
  const sent: Array<RpcOk | RpcError> = [];
  const coordLink = { send: (f: RpcOk | RpcError) => { sent.push(f); } } as unknown as CoordLink;
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

// The slice of the snapshot these tests read, named once rather than cast inline
// at each assertion. diagSnapshot's declared return is Record<string, unknown>;
// the per-session shape it builds is only expressible here.
interface SnapshotRaw {
  head_seq: number;
  tail_seq: number;
  retained_bytes: number;
  cap_bytes: number;
  evicting: boolean;
}
interface SnapshotCore {
  discarded: number;
  dropped: number;
  retained_lines: number;
  total: number;
}
interface SnapshotCell {
  grid_epoch: string;
  seq: number;
  sb_dropped: number;
  sb_origin: number;
  last_sb_total: number;
  core: SnapshotCore | null;
  origin_pin: (SbOriginPin & { age_ms: number }) | null;
}
interface SnapshotSession {
  raw: SnapshotRaw;
  cell: SnapshotCell;
}

function snapshotOf(mgr: SessionManager): SnapshotSession {
  const sessions = mgr.diagSnapshot().sessions as Record<string, SnapshotSession>;
  const session = sessions[String(SID)];
  if (session === undefined) throw new Error("session missing from diagnostic snapshot");
  return session;
}

describe("one snapshot compares the ring, the core, and the last emitted frame", () => {
  test("the core's live range is reported next to the frozen last-emitted one", async () => {
    const { mgr, rec } = await harness();
    feedLines(mgr, "TRUTH", 1, 1_300);
    mgr.emitCellSnapshot(asChannelId(CID));
    const emitted = snapshotOf(mgr);

    // At the emit instant the two agree, which is what makes a later disagreement
    // meaningful rather than noise.
    expect(emitted.cell.core).not.toBeNull();
    expect(emitted.cell.core!.dropped).toBe(emitted.cell.sb_dropped);
    expect(emitted.cell.core!.retained_lines).toBe(CORE_LINES);
    expect(emitted.cell.core!.total).toBe(emitted.cell.last_sb_total);
    expect(emitted.cell.core!.discarded).toBe(pushedBy(1_300) - CORE_LINES);
    // A fresh core owns its whole numbering, so Roost adds nothing to it yet.
    expect(emitted.cell.sb_origin).toBe(0);
    expect(emitted.cell.origin_pin).toBeNull();

    // Ring bounds: bytes, their cap, and the fact that eviction has begun — the
    // half of the divergence measured in a unit the core knows nothing about.
    expect(emitted.raw.cap_bytes).toBe(1024 * 1024);
    expect(emitted.raw.retained_bytes).toBe(rec.head_seq);
    expect(emitted.raw.evicting).toBe(false);
    expect(emitted.raw.tail_seq).toBe(0);

    // Now the eviction the emitted frame cannot see: no emit follows, so the
    // last-emitted numbering freezes while the core keeps discarding.
    const drift = 200;
    feedLines(mgr, "TRUTH", 1_301, drift);
    const live = snapshotOf(mgr);
    expect(live.cell.sb_dropped).toBe(emitted.cell.sb_dropped);
    expect(live.cell.last_sb_total).toBe(emitted.cell.last_sb_total);
    expect(live.cell.core!.dropped).toBe(emitted.cell.sb_dropped + drift);
    expect(live.cell.core!.total).toBe(emitted.cell.last_sb_total + drift);
    // And it is the LIVE value the read paths serve from.
    const page = await backfill(mgr, live.cell.core!.total, 10);
    expect(page.total).toBe(live.cell.core!.total);
    teardown(mgr);
  });
});

describe("a short page says WHICH history floor it hit", () => {
  test("a range the core still holds in full reports no floor at all", async () => {
    const { mgr } = await harness();
    feedLines(mgr, "TRUTH", 1, 1_300);
    const total = snapshotOf(mgr).cell.core!.total;

    const page = await backfill(mgr, total, 50);
    expect(page.rows).toHaveLength(50);
    expect(page.history_floor).toBe("none");
    teardown(mgr);
  });

  test("ordinary eviction is reported as gone forever", async () => {
    const { mgr, rec } = await harness();
    feedLines(mgr, "TRUTH", 1, 1_300);
    const floor = scrollbackOrigin(rec.wtermCore, rec.cell_emit);
    expect(floor).toBeGreaterThan(0);
    expect(rec.sb_origin_pin).toBeNull();

    // 400 rows ending 100 past the floor: only the surviving 100 exist.
    const page = await backfill(mgr, floor + 100, 400);
    expect(page.start_row).toBe(floor);
    expect(page.rows).toHaveLength(100);
    expect(page.history_floor).toBe("evicted");
    // The surviving rows are still the rows their indices claim.
    for (const row of page.rows) expect(markerOf(row, "TRUTH")).toBe(row.index + 1);
    teardown(mgr);
  });

  test("history a resize-forced replay could not reach is reported as resize_replay", async () => {
    // The byte ring, not the core, bounds a rebuild. Make it far too small to
    // reproduce the core's line ring and the fresh core comes back SHALLOWER —
    // history that was there a moment ago, lost to the replay bound rather than
    // to eviction.
    const { mgr, rec } = await harness(SMALL_RING);
    feedLines(mgr, "TRUTH", 1, 1_300);
    mgr.emitCellSnapshot(asChannelId(CID));
    const before = snapshotOf(mgr);
    expect(before.raw.evicting).toBe(true);
    expect(before.cell.core!.retained_lines).toBe(CORE_LINES);

    expect(await rebuildTerminalCore(mgr, CID, 100, ROWS, null)).toBe(true);

    const pin = snapshotOf(mgr).cell.origin_pin;
    if (pin === null) throw new Error("a rebuild did not record its origin pin");
    // Before/after values, in full, at the one moment the origin is DERIVED.
    expect(pin.prev_dropped).toBe(before.cell.core!.dropped);
    expect(pin.prev_total).toBe(before.cell.core!.total);
    expect(pin.replayed_ring).toBe(true);
    expect(pin.ring_evicted).toBe(false);
    expect(pin.clamped).toBe(false);
    expect(pin.cols).toBe(100);
    expect(pin.age_ms).toBeGreaterThanOrEqual(0);
    // The pin's invariant: the monotonic total does not rewind.
    expect(pin.sb_origin + pin.fresh_discarded + pin.fresh_count).toBe(pin.prev_total);
    // …and the floor JUMPED to buy it, which is the loss.
    expect(pin.fresh_count).toBeLessThan(CORE_LINES);
    expect(pin.sb_dropped).toBeGreaterThan(pin.prev_dropped);
    expect(pin.replay_lost_rows).toBe(pin.sb_dropped - pin.prev_dropped);
    expect(pin.replay_floor).toBe(pin.sb_dropped);

    const floor = scrollbackOrigin(rec.wtermCore, rec.cell_emit);
    expect(floor).toBe(pin.sb_dropped);
    const lost = await backfill(mgr, floor + 10, 400);
    expect(lost.start_row).toBe(floor);
    expect(lost.history_floor).toBe("resize_replay");
    // What SURVIVED still reports no floor and still names its own lines.
    const kept = await backfill(mgr, pin.prev_total, 50);
    expect(kept.history_floor).toBe("none");
    for (const row of kept.rows) expect(markerOf(row, "TRUTH")).toBe(row.index + 1);

    // Ordinary eviction past the replay's mark hands the floor back: the replay
    // no longer owns it, so claiming it does would be a lie.
    feedLines(mgr, "TRUTH", 1_301, 10_500);
    const evicted = scrollbackOrigin(rec.wtermCore, rec.cell_emit);
    expect(evicted).toBeGreaterThan(pin.replay_floor);
    const later = await backfill(mgr, evicted + 10, 400);
    expect(later.history_floor).toBe("evicted");
    teardown(mgr);
  });
});

describe("alt-screen bytes share the primary screen's byte ring", () => {
  // The finding, measured: a TUI dwell evicts pre-alt primary scrollback from the
  // byte ring while the core keeps every line of it. Nothing is wrong until the
  // session is resized — and then the rebuild can only replay what the ring still
  // holds, so history a never-resized session would still be serving is gone.
  const PRIMARY_LINES = 200;
  const ALT_FRAMES = 20;

  async function altDwell(capBytes?: number): Promise<Harness> {
    const h = await harness(capBytes);
    feedLines(h.mgr, "PRIMARY", 1, PRIMARY_LINES);
    feed(h.mgr, "\x1b[?1049h");
    expect(h.rec.alt_mode).toBe(true);
    for (let generation = 0; generation < ALT_FRAMES; generation++) feed(h.mgr, altFrame(generation));
    feed(h.mgr, "\x1b[?1049l");
    expect(h.rec.alt_mode).toBe(false);
    h.mgr.emitCellSnapshot(asChannelId(CID));
    return h;
  }

  test("the dwell evicts pre-alt primary bytes from the ring the core still holds as lines", async () => {
    const { mgr, rec } = await altDwell(SMALL_RING);

    // The core is untouched by all of this: alt-screen paint adds no scrollback
    // line, so every primary line is still there under its original index.
    expect(retainedMatching(rec, "PRIMARY")).toBe(pushedBy(PRIMARY_LINES));
    const snapshot = snapshotOf(mgr);
    expect(snapshot.cell.core!.dropped).toBe(0);
    expect(snapshot.cell.core!.total).toBe(pushedBy(PRIMARY_LINES));

    // The BYTE ring is the opposite story: the dwell lapped it many times over,
    // and not one pre-alt primary byte survived.
    const ringText = new TextDecoder().decode(readRing(rec.scrollback));
    expect(snapshot.raw.evicting).toBe(true);
    expect(snapshot.raw.retained_bytes).toBe(SMALL_RING);
    expect(ringText).not.toContain("PRIMARY-");
    expect(ringText).toContain("ALTPAINT");
    // Quantified: the dwell wrote many ring-fulls, so the eviction is not marginal.
    expect(rec.head_seq).toBeGreaterThan(SMALL_RING * 4);
    teardown(mgr);
  });

  test("so one resize after the dwell loses primary history a never-resized session keeps", async () => {
    const { mgr, rec } = await altDwell(SMALL_RING);
    const before = snapshotOf(mgr);
    expect(retainedMatching(rec, "PRIMARY")).toBe(pushedBy(PRIMARY_LINES));

    // A window drag. Not alt-primed — the TUI already exited — so the ring IS
    // replayed, and the ring is all that is left to replay.
    expect(await rebuildTerminalCore(mgr, CID, 100, ROWS, null)).toBe(true);

    expect(retainedMatching(rec, "PRIMARY")).toBe(0);
    const pin = snapshotOf(mgr).cell.origin_pin;
    if (pin === null) throw new Error("a rebuild did not record its origin pin");
    expect(pin.replayed_ring).toBe(true);
    expect(pin.prev_total).toBe(before.cell.core!.total);
    expect(pin.replay_lost_rows).toBe(pin.prev_total - pin.fresh_count);
    expect(pin.replay_lost_rows).toBeGreaterThan(0);
    // Every pre-alt row now answers with the honest reason: this was a resize, not
    // eviction, and the same session left un-resized would still hold these rows.
    const floor = scrollbackOrigin(rec.wtermCore, rec.cell_emit);
    const lost = await backfill(mgr, Math.min(floor + 10, pin.prev_total), 400);
    expect(lost.history_floor).toBe("resize_replay");
    teardown(mgr);
  });

  test("the control: the same dwell over a ring big enough to hold it loses nothing", async () => {
    // Same feed, same resize, only the ring's capacity differs — so the loss above
    // is attributable to alt bytes consuming the primary screen's byte budget, not
    // to the rebuild itself.
    const { mgr, rec } = await altDwell();
    const ringText = new TextDecoder().decode(readRing(rec.scrollback));
    expect(ringText).toContain("PRIMARY-1\r\n");

    expect(await rebuildTerminalCore(mgr, CID, 100, ROWS, null)).toBe(true);

    expect(retainedMatching(rec, "PRIMARY")).toBe(pushedBy(PRIMARY_LINES));
    const pin = snapshotOf(mgr).cell.origin_pin;
    if (pin === null) throw new Error("a rebuild did not record its origin pin");
    expect(pin.replay_lost_rows).toBe(0);
    expect(pin.replay_floor).toBe(0);
    const page = await backfill(mgr, pin.prev_total, 400);
    expect(page.history_floor).toBe("none");
    expect(page.start_row).toBe(0);
    for (const row of page.rows) expect(markerOf(row, "PRIMARY")).toBe(row.index + 1);
    teardown(mgr);
  });
});
