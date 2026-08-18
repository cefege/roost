// Find-in-scrollback epoch fence (terminalFindController.ts).
//
// A match `row` is an index into ONE worker-side grid numbering. A core rebuild
// re-pins the scrollback origin, so a row found in the retired epoch routinely
// stays numerically inside the new epoch's [sbBase, total) window while naming
// unrelated history — the old `row >= total` guard waves it straight through and
// the reader gets scrolled to someone else's output, labelled as their match.
// These lock the fail-closed behaviour:
//
//   F1 — a same-epoch hit pulls its row in and reveals it, exactly as before.
//   F2 — a hit from a retired epoch is DISCARDED (no jump, no stale highlights,
//        result set dropped) and re-searched against the grid now on screen —
//        with the numbers arranged so the pre-fix guard would have jumped.
//   F3 — a worker refusal after the pane reframed mid-flight re-asks once about
//        the epoch now displayed instead of blaming the query.
//   F4 — that retry is bounded: an epoch that keeps moving stops at one re-ask
//        and reports the failure rather than polling.
//   F5 — a refusal with the epoch UNCHANGED (bad regex, dead worker) still tints
//        immediately; the retry never swallows a real failure.

import { afterAll, beforeAll, beforeEach, describe, expect, mock, test } from "bun:test";
import type { CellGridRenderer } from "../src/lib/cellRenderer.ts";
import type { FindHit } from "../src/lib/cellRow.ts";
import type { ScrollbackBackfill } from "../src/lib/scrollbackBackfill.ts";

interface SearchRequest {
  sessionId: string;
  gridEpoch: string;
  query: string;
  caseSensitive: boolean;
  regex: boolean;
  maxMatches: number;
}
interface SearchResponse {
  matches: Array<{ row: bigint; col: number; len: number; preview: string }>;
  truncated: boolean;
  scrollbackTotal: bigint;
  cols: number;
  gridEpoch: string;
}

const requests: SearchRequest[] = [];
let rpcImpl: (request: SearchRequest) => Promise<SearchResponse>;

mock.module("../src/connect.ts", () => ({
  coordClient: {
    sessionsSearchScrollback(request: SearchRequest) {
      requests.push(request);
      return rpcImpl(request);
    },
  },
}));

// The Connect client must be mocked before the controller module is evaluated.
const { createTerminalFind } = await import("../src/lib/terminalFindController.ts");

const EPOCH_A = "grid-a:0";
const EPOCH_B = "grid-b:0";

function reply(rows: number[], gridEpoch: string): SearchResponse {
  return {
    matches: rows.map((row) => ({
      row: BigInt(row), col: 3, len: 4, preview: `line ${row}`,
    })),
    truncated: false,
    scrollbackTotal: 2000n,
    cols: 80,
    gridEpoch,
  };
}

interface Published { rows: number[]; active: { row: number; col: number } | null }

function harness() {
  // A deep-history pane: rows below 500 are reserved-but-unpainted.
  const anchor = { sbBase: 500, cols: 80, total: 2000, gridEpoch: EPOCH_A };
  const jumps: number[] = [];
  const pulled: number[] = [];
  const published: Published[] = [];
  let pullOk = true;
  const renderer = {
    backfillAnchor: () => ({ ...anchor }),
    setFindHighlights(
      hits: ReadonlyMap<number, FindHit[]>,
      active: { row: number; col: number } | null,
    ) {
      published.push({ rows: Array.from(hits.keys()), active });
    },
    scrollToScrollbackRow(absIndex: number) { jumps.push(absIndex); },
  } as unknown as CellGridRenderer;
  const backfill = {
    async ensureRowPainted(absIndex: number) {
      pulled.push(absIndex);
      return pullOk;
    },
  } as unknown as ScrollbackBackfill;
  const find = createTerminalFind({
    sessionId: "session-1",
    renderer: () => renderer,
    backfill: () => backfill,
  });
  return {
    anchor, jumps, pulled, published, find,
    setPullOk(value: boolean) { pullOk = value; },
    last(): Published {
      return published[published.length - 1] ?? { rows: [], active: null };
    },
  };
}

// Debounce control. The controller owns FIND_DEBOUNCE_MS; a test must not sleep
// through it, so setTimeout is captured and fired on demand.
const pendingTimers = new Map<number, () => void>();
let nextTimerId = 1;
const realSetTimeout = globalThis.setTimeout;
const realClearTimeout = globalThis.clearTimeout;

beforeAll(() => {
  Object.defineProperty(globalThis, "setTimeout", {
    configurable: true,
    value: (fn: () => void) => {
      const id = nextTimerId++;
      pendingTimers.set(id, fn);
      return id;
    },
  });
  Object.defineProperty(globalThis, "clearTimeout", {
    configurable: true,
    value: (id?: number) => { if (id !== undefined) pendingTimers.delete(id); },
  });
});
afterAll(() => {
  Object.defineProperty(globalThis, "setTimeout", { configurable: true, value: realSetTimeout });
  Object.defineProperty(globalThis, "clearTimeout", { configurable: true, value: realClearTimeout });
});

/** Drain the microtask chains an RPC + reveal walk through. */
async function settle(): Promise<void> {
  for (let i = 0; i < 12; i++) await Promise.resolve();
}

/** Fire the pending debounce and settle everything it starts. */
async function fireDebounce(): Promise<void> {
  const due = Array.from(pendingTimers.values());
  pendingTimers.clear();
  for (const fn of due) fn();
  await settle();
}

beforeEach(() => {
  requests.length = 0;
  pendingTimers.clear();
  rpcImpl = async () => reply([], EPOCH_A);
});

describe("terminal find epoch fence", () => {
  test("F1 — a same-epoch hit pulls its row in and reveals it", async () => {
    const h = harness();
    rpcImpl = async () => reply([120], EPOCH_A);

    h.find.setQuery("boom");
    await fireDebounce();

    expect(requests).toHaveLength(1);
    expect(requests[0]!.gridEpoch).toBe(EPOCH_A);
    expect(h.find.matches()).toHaveLength(1);
    expect(h.find.matches()[0]!.epoch).toBe(EPOCH_A);
    expect(h.find.index()).toBe(1);
    expect(h.find.failed()).toBe(false);
    // Reserved-but-unpainted row: pulled in, then jumped to.
    expect(h.pulled).toEqual([120]);
    expect(h.jumps).toEqual([120]);
    expect(h.last()).toEqual({ rows: [120], active: { row: 120, col: 3 } });
  });

  test("F2 — a hit from a retired epoch is discarded, never jumped to", async () => {
    const h = harness();
    rpcImpl = async () => reply([1200], EPOCH_A);

    h.find.setQuery("boom");
    await fireDebounce();
    expect(h.find.matches()).toHaveLength(1);
    expect(h.jumps).toEqual([1200]);
    h.jumps.length = 0;
    h.pulled.length = 0;

    // A rebuild: new epoch, origin re-pinned low. The held row stays INSIDE the
    // new window, so every pre-fix guard passes it — this is the wrong-row jump.
    h.anchor.gridEpoch = EPOCH_B;
    h.anchor.sbBase = 0;
    h.anchor.total = 1500;
    expect(1200).toBeGreaterThanOrEqual(h.anchor.sbBase);
    expect(1200).toBeLessThan(h.anchor.total);

    rpcImpl = async () => reply([80], EPOCH_B);
    h.find.step(1);
    await settle();

    // Nothing was revealed and nothing stale stayed painted.
    expect(h.jumps).not.toContain(1200);
    expect(h.pulled).not.toContain(1200);
    // The set was re-asked for the epoch now on screen, and that answer reveals.
    expect(requests).toHaveLength(2);
    expect(requests[1]!.gridEpoch).toBe(EPOCH_B);
    expect(h.find.matches().map((m) => [m.row, m.epoch])).toEqual([[80, EPOCH_B]]);
    expect(h.jumps).toEqual([80]);
    expect(h.last()).toEqual({ rows: [80], active: { row: 80, col: 3 } });
  });

  test("F2b — a stale set is dropped even when the re-search finds nothing", async () => {
    const h = harness();
    rpcImpl = async () => reply([1200], EPOCH_A);
    h.find.setQuery("boom");
    await fireDebounce();
    h.jumps.length = 0;

    h.anchor.gridEpoch = EPOCH_B;
    rpcImpl = async () => reply([], EPOCH_B);
    h.find.step(1);
    await settle();

    expect(h.jumps).toEqual([]);
    expect(h.find.matches()).toEqual([]);
    expect(h.find.index()).toBe(0);
    expect(h.last().rows).toEqual([]);
    expect(h.last().active).toBeNull();
    // Discarding a stale hit is not a search failure — the bar must not tint.
    expect(h.find.failed()).toBe(false);
  });

  test("F3 — a refused epoch re-asks once about the grid now on screen", async () => {
    const h = harness();
    rpcImpl = async (request) => {
      if (request.gridEpoch === EPOCH_A) {
        // The pane reframed while this scan was in flight; the worker refuses the
        // epoch the request named.
        h.anchor.gridEpoch = EPOCH_B;
        throw new Error("grid epoch changed");
      }
      return reply([700], EPOCH_B);
    };

    h.find.setQuery("boom");
    await fireDebounce();

    expect(requests.map((r) => r.gridEpoch)).toEqual([EPOCH_A, EPOCH_B]);
    expect(h.find.failed()).toBe(false);
    expect(h.find.matches().map((m) => m.row)).toEqual([700]);
    // 700 is already painted (>= sbBase 500), so it is revealed with no pull.
    expect(h.pulled).toEqual([]);
    expect(h.jumps).toEqual([700]);
  });

  test("F4 — the re-ask is bounded when the epoch keeps moving", async () => {
    const h = harness();
    let flip = 0;
    rpcImpl = async () => {
      h.anchor.gridEpoch = `grid-${++flip}:0`;
      throw new Error("grid epoch changed");
    };

    h.find.setQuery("boom");
    await fireDebounce();

    // One retry, then an honest failure — never a poll.
    expect(requests).toHaveLength(2);
    expect(h.find.failed()).toBe(true);
    expect(h.find.matches()).toEqual([]);
    expect(h.find.index()).toBe(0);
  });

  test("F5 — a failure with the epoch unchanged tints without retrying", async () => {
    const h = harness();
    rpcImpl = async () => { throw new Error("invalid regex: nothing to repeat"); };

    h.find.setQuery("*");
    await fireDebounce();

    expect(requests).toHaveLength(1);
    expect(h.find.failed()).toBe(true);
    expect(h.find.matches()).toEqual([]);
    expect(h.jumps).toEqual([]);
  });
});
