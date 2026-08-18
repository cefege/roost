import { describe, test, expect, beforeAll } from "bun:test";
import {
  CID,
  FP_A,
  FP_B,
  FP_C,
  freshMgr,
  injectSession,
  readApplied,
  afterWithdraw,
  ready,
} from "./multi-viewer-dynamic-helpers.ts";

beforeAll(async () => { await ready; });

describe("multi-viewer dynamic — N-viewer churn", () => {
  test("7A — three viewers, mid-sized leaves → SCD recomputes to min(remaining)", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 200, 60);
    mgr.claimViewport(CID, FP_B, 120, 40);
    mgr.claimViewport(CID, FP_C, 90, 28);
    mgr.withdrawViewport(CID, FP_B);
    expect(await readApplied(mgr)).toEqual({ cols: 90, rows: 28 });
  });

  test("7B — three viewers, smallest leaves → SCD grows to next-min", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 200, 60);
    mgr.claimViewport(CID, FP_B, 120, 40);
    mgr.claimViewport(CID, FP_C, 90, 28);
    mgr.withdrawViewport(CID, FP_C);
    await afterWithdraw();
    expect(await readApplied(mgr)).toEqual({ cols: 120, rows: 40 });
  });

  test("7C — all viewers leave one-by-one → SCD freezes at last pinning value", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 200, 60);
    mgr.claimViewport(CID, FP_B, 90, 28);
    expect(await readApplied(mgr)).toEqual({ cols: 90, rows: 28 });
    mgr.withdrawViewport(CID, FP_A);
    expect(await readApplied(mgr)).toEqual({ cols: 90, rows: 28 });
    mgr.withdrawViewport(CID, FP_B);
    expect(await readApplied(mgr)).toEqual({ cols: 90, rows: 28 });
  });

  test("7D — orphaned claims (session gone) are cleaned on next withdraw or reap", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 200, 60);
    mgr.claimViewport(CID, FP_B, 90, 28);
    // Session vanishes without going through clearViewportState
    // (kill -9 keeper / WASM panic / etc).
    (mgr as unknown as { sessions: Map<number, unknown> }).sessions.delete(CID);
    // The deferred withdraw fires _recomputeViewport which hits the orphan
    // branch and drops the whole channel's claim map + applied cache.
    mgr.withdrawViewport(CID, FP_A);
    await afterWithdraw();
    const claims = (mgr as unknown as {
      viewportClaims: Map<number, unknown>;
    }).viewportClaims.get(CID);
    expect(claims).toBeUndefined();
    expect(await readApplied(mgr)).toBeUndefined();
  });

  test("7E — 10-viewer stress: SCD = min across all; each withdraw recomputes correctly", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    // 10 viewers with dims [200x60, 190x58, ..., 110x42]. Min = (110, 42).
    const fps: string[] = [];
    for (let i = 0; i < 10; i++) {
      const fp = String.fromCharCode(0x40 + i).repeat(64);
      fps.push(fp);
      mgr.claimViewport(CID, fp, 200 - i * 10, 60 - i * 2);
    }
    expect(await readApplied(mgr)).toEqual({ cols: 110, rows: 42 });
    // Withdraw the smallest claim → SCD grows to the second-smallest.
    mgr.withdrawViewport(CID, fps[9]!);
    await afterWithdraw();
    expect(await readApplied(mgr)).toEqual({ cols: 120, rows: 44 });
    // Withdraw remaining except viewers[8], ensure SCD is always min(live).
    const remaining = fps.slice(0, 9);
    for (let i = 0; i < remaining.length - 1; i++) {
      mgr.withdrawViewport(CID, remaining[i]!);
    }
    await afterWithdraw();
    // Only viewers[8] left → SCD = (200-80, 60-16) = (120, 44).
    expect(await readApplied(mgr)).toEqual({ cols: 120, rows: 44 });
  });
});

describe("multi-viewer dynamic — race / interleave", () => {
  test("R1 — interleaved claims from N viewers converge deterministically", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    const claims: Array<{ fp: string; cols: number; rows: number }> = [
      { fp: FP_A, cols: 200, rows: 60 },
      { fp: FP_B, cols: 100, rows: 30 },
      { fp: FP_C, cols: 150, rows: 45 },
      { fp: FP_A, cols: 80, rows: 24 },   // A shrinks
      { fp: FP_C, cols: 60, rows: 20 },   // C shrinks below A
    ];
    for (const c of claims) mgr.claimViewport(CID, c.fp, c.cols, c.rows);
    // Final live claims: A=80x24, B=100x30, C=60x20 → SCD = (60, 20).
    expect(await readApplied(mgr)).toEqual({ cols: 60, rows: 20 });
  });

  test("R3 — concurrent withdraws from two distinct fps are commutative", async () => {
    const order1 = freshMgr();
    await injectSession(order1, 80, 24);
    order1.claimViewport(CID, FP_A, 200, 60);
    order1.claimViewport(CID, FP_B, 100, 30);
    order1.claimViewport(CID, FP_C, 80, 24);
    order1.withdrawViewport(CID, FP_A);
    order1.withdrawViewport(CID, FP_B);

    const order2 = freshMgr();
    await injectSession(order2, 80, 24);
    order2.claimViewport(CID, FP_A, 200, 60);
    order2.claimViewport(CID, FP_B, 100, 30);
    order2.claimViewport(CID, FP_C, 80, 24);
    order2.withdrawViewport(CID, FP_B);
    order2.withdrawViewport(CID, FP_A);

    expect(await readApplied(order1)).toEqual(await readApplied(order2));
  });
});
