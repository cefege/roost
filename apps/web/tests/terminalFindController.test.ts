// Terminal-find epoch-fence tests: a hit is only revealed against the grid it
// was found in, a retired epoch is discarded and re-searched once, and a refused
// or mid-chain epoch move spends exactly one retry.
// Page-chain cursor behaviour lives in terminalFindController-paging.test.ts;
// the mocked RPC and debounce clock are terminalFindController-test-harness.ts.

import { describe, expect, test } from "bun:test";
import { SearchStopReason } from "@roost/shared/proto/coordinator_pb";
import {
  EPOCH_A, EPOCH_B, fireDebounce, createFindHarness, installTerminalFindTestLifecycle,
  reply, requests, setSearchRpc, settle,
} from "./terminalFindController-test-harness.ts";

const { createTerminalFind } = await import("../src/lib/terminalFindController.ts");

function harness() {
  return createFindHarness(createTerminalFind);
}

installTerminalFindTestLifecycle();

describe("terminal find paging and epoch fence", () => {
  test("F1 — a same-epoch hit pulls its row in and reveals it", async () => {
    const h = harness();
    setSearchRpc(async () => reply([120], EPOCH_A));
    h.find.setQuery("boom");
    await fireDebounce();
    expect(requests).toHaveLength(1);
    expect(requests[0]!.gridEpoch).toBe(EPOCH_A);
    expect(h.find.matches().map((match) => [match.row, match.epoch])).toEqual([[120, EPOCH_A]]);
    expect(h.find.index()).toBe(1);
    expect(h.pulled).toEqual([120]);
    expect(h.jumps).toEqual([120]);
    expect(h.last()).toEqual({ rows: [120], active: { row: 120, col: 3 } });
  });

  test("F2 — a retired-epoch set is discarded and re-searched before reveal", async () => {
    const h = harness();
    setSearchRpc(async () => reply([1200], EPOCH_A));
    h.find.setQuery("boom");
    await fireDebounce();
    h.jumps.length = 0;
    h.anchor.gridEpoch = EPOCH_B;
    h.anchor.sbBase = 0;
    h.anchor.total = 1500;
    setSearchRpc(async () => reply([80], EPOCH_B));
    h.find.step(1);
    await settle();
    expect(requests.map((request) => request.gridEpoch)).toEqual([EPOCH_A, EPOCH_B]);
    expect(h.jumps).not.toContain(1200);
    expect(h.find.matches().map((match) => [match.row, match.epoch])).toEqual([[80, EPOCH_B]]);
    expect(h.jumps).toEqual([80]);
  });

  test("F2b — a stale set stays discarded when the retry finds nothing", async () => {
    const h = harness();
    setSearchRpc(async () => reply([1200], EPOCH_A));
    h.find.setQuery("boom");
    await fireDebounce();
    h.jumps.length = 0;
    h.anchor.gridEpoch = EPOCH_B;
    setSearchRpc(async () => reply([], EPOCH_B));
    h.find.step(1);
    await settle();
    expect(h.jumps).toEqual([]);
    expect(h.find.matches()).toEqual([]);
    expect(h.find.index()).toBe(0);
    expect(h.last()).toEqual({ rows: [], active: null });
    expect(h.find.failed()).toBe(false);
  });

  test("F3 — a refused moved epoch re-asks once against the displayed grid", async () => {
    const h = harness();
    setSearchRpc(async (request) => {
      if (request.gridEpoch === EPOCH_A) {
        h.anchor.gridEpoch = EPOCH_B;
        throw new Error("grid epoch changed");
      }
      return reply([700], EPOCH_B);
    });
    h.find.setQuery("boom");
    await fireDebounce();
    expect(requests.map((request) => request.gridEpoch)).toEqual([EPOCH_A, EPOCH_B]);
    expect(h.find.failed()).toBe(false);
    expect(h.find.matches().map((match) => match.row)).toEqual([700]);
    expect(h.jumps).toEqual([700]);
  });

  test("F4 — a repeatedly moving epoch spends only one retry", async () => {
    const h = harness();
    let flip = 0;
    setSearchRpc(async () => {
      h.anchor.gridEpoch = `grid-${++flip}:0`;
      throw new Error("grid epoch changed");
    });
    h.find.setQuery("boom");
    await fireDebounce();
    expect(requests).toHaveLength(2);
    expect(h.find.failed()).toBe(true);
    expect(h.find.matches()).toEqual([]);
  });

  test("F5 — an ordinary RPC or regex failure does not retry", async () => {
    const h = harness();
    setSearchRpc(async () => { throw new Error("invalid regex"); });
    h.find.setQuery("*");
    await fireDebounce();
    expect(requests).toHaveLength(1);
    expect(h.find.failed()).toBe(true);
    expect(h.find.matches()).toEqual([]);
    expect(h.jumps).toEqual([]);
  });

  test("later-page epoch change discards the chain and retries from newest", async () => {
    const h = harness();
    let call = 0;
    setSearchRpc(async () => {
      call++;
      if (call === 1) {
        return reply([1500], EPOCH_A, {
          stop: SearchStopReason.ROW_LIMIT, start: 1000, end: 2000, next: 1000,
        });
      }
      if (call === 2) {
        h.anchor.gridEpoch = EPOCH_B;
        return reply([900], EPOCH_A, { stop: SearchStopReason.EPOCH_CHANGED });
      }
      return reply([80], EPOCH_B);
    });
    h.find.setQuery("moving");
    await fireDebounce();
    expect(requests.map((request) => request.gridEpoch)).toEqual([EPOCH_A, EPOCH_A, EPOCH_B]);
    expect(requests.map((request) => request.beforeRow)).toEqual([undefined, 1000n, undefined]);
    expect(h.find.matches().map((match) => [match.row, match.epoch])).toEqual([[80, EPOCH_B]]);
    expect(h.find.failed()).toBe(false);
  });
});
