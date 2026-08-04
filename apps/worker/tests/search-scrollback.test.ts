// Find-in-scrollback (search-scrollback RPC) against a REAL wterm core.
//
// The SPA holds at most 2000 of the worker's retained rows, so this walk is
// the only complete search — and its `row` is the MONOTONIC absolute index the
// SPA jumps its reader to. An off-by-one lands the user on the wrong line, so
// S1 cross-checks the returned index back through readScrollbackRangeCells
// (the mapping the backfill RPC and every cell frame already use) AND against
// the marker's own line number, which is independent of that reader.
//
//   S1 — a mid-history marker returns its exact absolute index + preview.
//   S2 — max_matches clamps the result set and reports truncated.
//   S3 — an invalid regex rejects with the `invalid regex: ` prefix coord maps
//        to Code.InvalidArgument; a valid one still answers.
//   S4 — the slice yield really yields: work scheduled after the search starts
//        runs before the search resolves, so other sessions' PTY output keeps
//        flowing during a full-depth scan (CLAUDE.md L11).
//
// Everything is derived from the core at run time: the factory falls back to
// the stock 1k-line wasm when the patched 10k build is unreadable, and this
// suite must pin the same contract on either. The scan order is scrollback
// (newest retained line first) and then the live viewport, so a truncated
// result never contains viewport rows.

import { describe, test, expect } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { handleSearchScrollback } from "../src/browser-command-terminal.ts";
import type { CoordLink } from "../src/transport/CoordLink.ts";
import type { SessionShellRecord } from "../src/session-record.ts";
import type { FsmChannel } from "../src/fsm.ts";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared";
import type { ClientControlFrame } from "@roost/shared/wire";
import { initCellEmitState, readScrollbackRangeCells, type CellRow } from "@roost/shared/cell";
import { createWtermCore } from "@roost/shared/wterm-core-factory";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";

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
  data: { matches: SearchMatch[]; truncated: boolean; total: number; cols: number };
}
interface RpcErr { kind: "rpc-error"; request_id: string; message: string }

function freshMgr(): SessionManager {
  return new SessionManager({ workerFp: asWorkerFp("00".repeat(32)), sink: { emit: () => {} } });
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
    fsm,
    scrollback: createSbRing(new Uint8Array(SEED)),
    head_seq: SEED.length,
    alt_mode: false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    ...initAgentOscState(),
    wtermCore,
    session_trace_id: "sbfind00",
    cell_emit: initCellEmitState(),
    lastPtyOutMs: 0,
    spawnedAtMs: Date.now(),
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
  opts: { caseSensitive?: boolean; regex?: boolean; maxMatches?: number } = {},
): Extract<ClientControlFrame, { kind: "search-scrollback" }> {
  return {
    kind: "search-scrollback",
    request_id: "req",
    session_id: SID,
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
});
