// These tests exercise complete find-in-scrollback scans against a real terminal core.
// They cross-check monotonic row indices through the same reader used by browser backfill.
// Cases cover limits, regex errors, event-loop yielding, wide glyphs, and epoch fencing.
// Runtime-derived core depth keeps the assertions valid across retained-history capacities.

import { describe, test, expect } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { handleSearchScrollback } from "../src/browser-command-terminal.ts";
import type { CoordLink } from "../src/transport/coord-link.ts";
import type { SessionShellRecord } from "../src/session-record.ts";
import type { FsmChannel } from "../src/fsm.ts";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared/wire";
import type { ClientControlFrame } from "@roost/shared/wire";
import {
  cellGridEpoch, initCellEmitState, readScrollbackRangeCells, type CellRow,
} from "@roost/shared/cell";
import { createWtermCore } from "@roost/shared/wterm-core-factory";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";
import { keeperTestShellSpec } from "./keeper-test-fixtures.ts";
import { LifecycleTestSink } from "./lifecycle-test-sink.ts";

const SID = asSessionId("00000000-0000-0000-0000-000000000001");
const CID = 1;
const COLS = 80, ROWS = 24;
const LINE_COUNT = 3000;
// 3000 short numbered lines: more rows than several SEARCH_SLICE_ROWS (500)
// slices on either core depth, so S4 exercises the real yield path.
const SEED = new TextEncoder().encode(
  Array.from({ length: LINE_COUNT }, (_, i) => `FINDLINE-${i}`).join("\r\n") + "\r\n",
);

const rowTextOf = (r: CellRow): string => r.spans.map((s) => s.text).join("");

interface SearchMatch { row: number; col: number; len: number; preview: string }
interface RpcOk {
  kind: "rpc-ok";
  request_id: string;
  data: {
    matches: SearchMatch[]; truncated: boolean; total: number; cols: number;
    grid_epoch: string;
  };
}
interface RpcErr { kind: "rpc-error"; request_id: string; message: string }

function freshMgr(): SessionManager {
  return new SessionManager({ workerFp: asWorkerFp("00".repeat(32)), sink: new LifecycleTestSink() });
}

async function injectSession(mgr: SessionManager): Promise<SessionShellRecord> {
  const wtermCore = await createWtermCore(COLS, ROWS);
  wtermCore.writeRaw(SEED);
  // Test double: nothing on the search path touches the FSM.
  const fsm = {} as unknown as FsmChannel;
  const record: SessionShellRecord = {
    sessionId: SID,
    channelId: asChannelId(CID),
    socketPath: "/dev/null",
    kind: "shell",
    cwd: "/",
    shellSpec: keeperTestShellSpec({ executable: process.execPath, cwd: "/" }),
    fsm,
    scrollback: createSbRing(new Uint8Array(SEED)),
    head_seq: SEED.length,
    alt_mode: false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    query_carry: new Uint8Array(0),
    ...initAgentOscState(),
    wtermCore,
    session_trace_id: "sbfind00",
    cell_emit: initCellEmitState("test-grid", "00000000-0000-4000-8000-000000000001"),
    lastPtyOutMs: 0,
    sb_origin_pin: null,
    spawnedAtMs: Date.now(),
    closeReservation: mgr.reserveLifecycleEvent("closed"),
  };
  mgr.sessions.set(CID, record);
  return record;
}

function makeLinkCapture(): { coordLink: CoordLink; sent: Array<RpcOk | RpcErr> } {
  const sent: Array<RpcOk | RpcErr> = [];
  const stub = { send: (f: RpcOk | RpcErr) => { sent.push(f); } };
  // Test double: handleSearchScrollback only calls coordLink.send.
  const coordLink = stub as unknown as CoordLink;
  return { coordLink, sent };
}

function searchFrame(
  query: string,
  opts: {
    caseSensitive?: boolean; regex?: boolean; maxMatches?: number; gridEpoch?: string;
  } = {},
): Extract<ClientControlFrame, { kind: "search-scrollback" }> {
  return {
    kind: "search-scrollback",
    request_id: "req",
    session_id: SID,
    // "" binds to the worker's current epoch — the headless/API path. Tests that
    // care about the fence name an epoch explicitly.
    grid_epoch: opts.gridEpoch ?? "",
    query,
    case_sensitive: opts.caseSensitive ?? false,
    regex: opts.regex ?? false,
    max_matches: opts.maxMatches ?? 500,
  };
}

/** Plain text of one scrollback row by MONOTONIC absolute index, through the
 *  same reader the SPA's backfill uses. */
function readRowText(rec: SessionShellRecord, absIndex: number): string {
  const [row] = readScrollbackRangeCells(
    rec.wtermCore, absIndex, absIndex + 1, rec.cell_emit.sbDropped,
  );
  expect(row!.index).toBe(absIndex);
  return rowTextOf(row!).trimEnd();
}

const markerNumber = (text: string): number => Number(text.slice("FINDLINE-".length));

describe("search-scrollback", () => {
  test("S1 — a mid-history marker returns its exact absolute index", async () => {
    const mgr = freshMgr();
    const rec = await injectSession(mgr);
    const { coordLink, sent } = makeLinkCapture();
    const sbDropped = rec.cell_emit.sbDropped;
    const retained = rec.wtermCore.getScrollbackCount();
    const monoTotal = sbDropped + retained;
    // Several slices deep, whichever wasm depth the factory produced.
    expect(retained).toBeGreaterThan(900);

    // Take the query FROM the grid so the assertion cannot drift with the
    // ring's retention depth: the middle retained line is unique history.
    const midIndex = sbDropped + Math.floor(retained / 2);
    const marker = readRowText(rec, midIndex);
    const oldest = markerNumber(readRowText(rec, sbDropped));

    await handleSearchScrollback(searchFrame(marker), "req", { coordLink, sessionMgr: mgr });
    expect(sent[0]!.kind).toBe("rpc-ok");
    const ok = sent[0] as RpcOk;
    expect(ok.data.total).toBe(monoTotal);
    expect(ok.data.cols).toBe(COLS);
    expect(ok.data.truncated).toBe(false);
    // "FINDLINE-<n>" is not a prefix of any other line below LINE_COUNT.
    expect(ok.data.matches.length).toBe(1);

    const hit = ok.data.matches[0]!;
    expect(hit.row).toBe(midIndex);
    expect(hit.col).toBe(0);
    expect(hit.len).toBe(marker.length);
    expect(hit.preview).toContain(marker);
    // Independent arithmetic check of the SAME index, not routed through the
    // reader: consecutive markers occupy consecutive absolute indices.
    expect(hit.row).toBe(sbDropped + (markerNumber(marker) - oldest));
    // Neighbours confirm it is not off by one in either direction.
    expect(markerNumber(readRowText(rec, hit.row - 1))).toBe(markerNumber(marker) - 1);
    expect(markerNumber(readRowText(rec, hit.row + 1))).toBe(markerNumber(marker) + 1);
  });

  test("S2 — max_matches clamps the result set and reports truncated", async () => {
    const mgr = freshMgr();
    const rec = await injectSession(mgr);
    const { coordLink, sent } = makeLinkCapture();
    const newestScrollback = rec.cell_emit.sbDropped + rec.wtermCore.getScrollbackCount() - 1;

    await handleSearchScrollback(
      searchFrame("FINDLINE-", { maxMatches: 10 }), "req", { coordLink, sessionMgr: mgr },
    );
    const ok = sent[0] as RpcOk;
    expect(ok.kind).toBe("rpc-ok");
    expect(ok.data.matches.length).toBe(10);
    expect(ok.data.truncated).toBe(true);
    // Newest-first through the ring, so an early cutoff keeps recent history:
    // the first hit is the newest RETAINED SCROLLBACK line (the viewport is
    // scanned last and never reached under a cutoff) and indices descend.
    expect(ok.data.matches[0]!.row).toBe(newestScrollback);
    expect(ok.data.matches[0]!.preview).toBe(readRowText(rec, newestScrollback));
    for (let i = 1; i < ok.data.matches.length; i++) {
      expect(ok.data.matches[i]!.row).toBe(newestScrollback - i);
    }
  });

  test("S3 — an invalid regex rejects with the invalid regex: prefix", async () => {
    const mgr = freshMgr();
    const rec = await injectSession(mgr);
    const { coordLink, sent } = makeLinkCapture();

    await handleSearchScrollback(
      searchFrame("[", { regex: true }), "req", { coordLink, sessionMgr: mgr },
    );
    expect(sent.length).toBe(1);
    expect(sent[0]!.kind).toBe("rpc-error");
    expect((sent[0] as RpcErr).message.startsWith("invalid regex: ")).toBe(true);

    // A valid pattern over the same grid still answers, at the same index.
    // `(\D|$)` not `$`: wterm reports a padded length for a retained line, so
    // the row text usually carries trailing spaces and a bare `$` would never
    // anchor. This form pins the exact marker under either behaviour.
    const midIndex = rec.cell_emit.sbDropped + Math.floor(rec.wtermCore.getScrollbackCount() / 2);
    const marker = readRowText(rec, midIndex);
    sent.length = 0;
    await handleSearchScrollback(
      searchFrame(`^FINDLINE-${markerNumber(marker)}(\\D|$)`, { regex: true }),
      "req", { coordLink, sessionMgr: mgr },
    );
    const ok = sent[0] as RpcOk;
    expect(ok.kind).toBe("rpc-ok");
    expect(ok.data.matches.length).toBe(1);
    expect(ok.data.matches[0]!.row).toBe(midIndex);
    expect(ok.data.matches[0]!.col).toBe(0);
  });

  test("S4 — the slice yield keeps the event loop serving other sessions", async () => {
    const mgr = freshMgr();
    await injectSession(mgr);
    const { coordLink, sent } = makeLinkCapture();

    // A full-depth scan with no early cutoff (nothing matches) crosses several
    // SEARCH_SLICE_ROWS boundaries. Work queued once the search is already
    // running must land BEFORE it resolves, or the scan monopolised the loop.
    const order: string[] = [];
    const search = handleSearchScrollback(
      searchFrame("NO-SUCH-MARKER"), "req", { coordLink, sessionMgr: mgr },
    ).then(() => { order.push("search"); });
    const racer = new Promise<void>((resolve) => { setImmediate(resolve); })
      .then(() => { order.push("racer"); });

    await Promise.all([search, racer]);
    expect(order).toEqual(["racer", "search"]);
    const ok = sent[0] as RpcOk;
    expect(ok.kind).toBe("rpc-ok");
    expect(ok.data.matches.length).toBe(0);
    expect(ok.data.truncated).toBe(false);
  });

  test("S5 — a live viewport match indexes above scrollbackTotal", async () => {
    const mgr = freshMgr();
    const rec = await injectSession(mgr);
    const { coordLink, sent } = makeLinkCapture();
    const core = rec.wtermCore;
    const monoTotal = rec.cell_emit.sbDropped + core.getScrollbackCount();

    // The newest markers never entered the ring — they are still on the live
    // grid. Locate one directly so the expected index is built from the core,
    // not from the handler's own arithmetic.
    const newest = `FINDLINE-${LINE_COUNT - 1}`;
    let viewportRow = -1;
    for (let row = 0; row < ROWS && viewportRow < 0; row++) {
      let text = "";
      for (let col = 0; col < COLS; col++) {
        const cp = core.getCell(row, col).char;
        text += cp === 0 ? " " : String.fromCodePoint(cp);
      }
      if (text.trimEnd() === newest) viewportRow = row;
    }
    expect(viewportRow).toBeGreaterThanOrEqual(0);

    await handleSearchScrollback(searchFrame(newest), "req", { coordLink, sessionMgr: mgr });
    const ok = sent[0] as RpcOk;
    expect(ok.kind).toBe("rpc-ok");
    expect(ok.data.matches.length).toBe(1);
    const hit = ok.data.matches[0]!;
    // The client tests `row >= scrollbackTotal` to decide a match is already
    // on screen and needs no scroll; a viewport row landing inside the
    // scrollback range would send it scrolling to a row that does not exist.
    expect(hit.row).toBe(monoTotal + viewportRow);
    expect(hit.row).toBeGreaterThanOrEqual(ok.data.total);
    expect(hit.preview).toContain(newest);
  });

  test("S6 — matches on a row with wide glyphs are reported in GRID columns", async () => {
    const mgr = freshMgr();
    const rec = await injectSession(mgr);
    const { coordLink, sent } = makeLinkCapture();
    // Grid columns of this row: 开=0-1 始=2-3 ' '=4 中=5-6 文=7-8 ' '=9 e=10 n=11 d=12.
    rec.wtermCore.writeRaw(new TextEncoder().encode("开始 中文 end\r\n"));

    await handleSearchScrollback(searchFrame("中文"), "req", { coordLink, sessionMgr: mgr });
    const ok = sent[0] as RpcOk;
    expect(ok.kind).toBe("rpc-ok");
    expect(ok.data.matches.length).toBe(1);
    const hit = ok.data.matches[0]!;
    // Text offset 3, but grid column 5 — and two characters occupying FOUR
    // columns. The SPA highlights `len` columns from `col`, so reporting the
    // text offsets here would mark "始 中" instead.
    expect(hit.col).toBe(5);
    expect(hit.len).toBe(4);

    // Tripwire for the continuation-as-space regression: if a wide glyph's
    // width-0 cell were read as a blank column again, the row's text would be
    // "开始  中 文  end" and THIS is what would match instead.
    const phantom = makeLinkCapture();
    await handleSearchScrollback(
      searchFrame("中 文"), "req", { coordLink: phantom.coordLink, sessionMgr: mgr },
    );
    expect((phantom.sent[0] as RpcOk).data.matches.length).toBe(0);

    // A match that starts on a narrow cell and ends inside a wide one still
    // names whole columns: "始 中" is offsets 1..4 → columns 2 through 7.
    const spanning = makeLinkCapture();
    await handleSearchScrollback(
      searchFrame("始 中"), "req", { coordLink: spanning.coordLink, sessionMgr: mgr },
    );
    const spanHit = (spanning.sent[0] as RpcOk).data.matches[0]!;
    expect([spanHit.col, spanHit.len]).toEqual([2, 5]);
  });

  test("S7 — a stale epoch is refused, not answered from the rebuilt core", async () => {
    const mgr = freshMgr();
    const rec = await injectSession(mgr);
    const heldEpoch = cellGridEpoch(rec.cell_emit);

    // The hit a client holds, found under the numbering it displays.
    const midIndex = rec.cell_emit.sbDropped + Math.floor(rec.wtermCore.getScrollbackCount() / 2);
    const marker = readRowText(rec, midIndex);
    const held = makeLinkCapture();
    await handleSearchScrollback(
      searchFrame(marker, { gridEpoch: heldEpoch }), "req",
      { coordLink: held.coordLink, sessionMgr: mgr },
    );
    const first = held.sent[0] as RpcOk;
    expect(first.kind).toBe("rpc-ok");
    expect(first.data.grid_epoch).toBe(heldEpoch);
    expect(first.data.matches[0]!.row).toBe(midIndex);

    // A resize rebuild: fresh core, fresh epoch base, origin pinned at 0 — what
    // session-resize-capture.ts produces. Different content, same index space.
    const rebuilt = await createWtermCore(COLS, ROWS);
    rebuilt.writeRaw(new TextEncoder().encode(
      Array.from({ length: LINE_COUNT }, (_, i) => `REBUILT-${i}`).join("\r\n") + "\r\n",
    ));
    rec.wtermCore = rebuilt;
    rec.cell_emit = initCellEmitState("rebuilt-grid", "00000000-0000-4000-8000-000000000001");
    const freshEpoch = cellGridEpoch(rec.cell_emit);
    expect(freshEpoch).not.toBe(heldEpoch);

    // The row the client holds is INSIDE the rebuilt core's valid range, naming
    // unrelated content — the precondition for the silent wrong-row jump.
    const rebuiltIndex = rec.cell_emit.sbDropped + Math.floor(rebuilt.getScrollbackCount() / 2);
    expect(midIndex).toBeLessThan(rec.cell_emit.sbDropped + rebuilt.getScrollbackCount());
    const rebuiltMarker = readRowText(rec, rebuiltIndex);
    expect(rebuiltMarker).not.toBe(marker);

    // The exact query the rebuilt core WOULD answer, carrying the stale epoch.
    const stale = makeLinkCapture();
    await handleSearchScrollback(
      searchFrame(rebuiltMarker, { gridEpoch: heldEpoch }), "req",
      { coordLink: stale.coordLink, sessionMgr: mgr },
    );
    expect(stale.sent.length).toBe(1);
    expect(stale.sent[0]!.kind).toBe("rpc-error");
    expect((stale.sent[0] as RpcErr).message).toBe("grid epoch changed");

    // Not a blanket refusal: the numbering now being served answers normally.
    const current = makeLinkCapture();
    await handleSearchScrollback(
      searchFrame(rebuiltMarker, { gridEpoch: freshEpoch }), "req",
      { coordLink: current.coordLink, sessionMgr: mgr },
    );
    const ok = current.sent[0] as RpcOk;
    expect(ok.kind).toBe("rpc-ok");
    expect(ok.data.grid_epoch).toBe(freshEpoch);
    expect(ok.data.matches.length).toBe(1);
    expect(ok.data.matches[0]!.row).toBe(rebuiltIndex);
  });

  test("S8 — a reframe mid-scan stops the walk instead of mixing numberings", async () => {
    const mgr = freshMgr();
    const rec = await injectSession(mgr);
    const { coordLink, sent } = makeLinkCapture();
    const servingEpoch = cellGridEpoch(rec.cell_emit);

    // A full-depth scan for a marker that does not exist crosses several
    // SEARCH_SLICE_ROWS boundaries and, undisturbed, reports truncated:false (S4).
    const search = handleSearchScrollback(
      searchFrame("NO-SUCH-MARKER", { gridEpoch: servingEpoch }), "req",
      { coordLink, sessionMgr: mgr },
    );
    // Land a semantic reframe on a slice boundary: revision bump, SAME core, so
    // only the epoch comparison can catch it.
    await new Promise<void>((resolve) => { setImmediate(resolve); });
    rec.cell_emit = {
      ...rec.cell_emit,
      gridEpochRevision: rec.cell_emit.gridEpochRevision + 1,
    };
    await search;

    const ok = sent[0] as RpcOk;
    expect(ok.kind).toBe("rpc-ok");
    expect(ok.data.truncated).toBe(true);
    // Whatever it scanned belongs to the epoch it names, never the new one.
    expect(ok.data.grid_epoch).toBe(servingEpoch);
    expect(cellGridEpoch(rec.cell_emit)).not.toBe(servingEpoch);
  });
});
