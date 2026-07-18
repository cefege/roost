import { describe, test, expect, beforeAll } from "bun:test";
import {
  CID,
  FP_A,
  FP_B,
  FP_C,
  freshMgr,
  injectSession,
  readApplied,
  ageClaim,
  reapNow,
  afterWithdraw,
  ready,
} from "./multi-viewer-dynamic-helpers.ts";

beforeAll(async () => { await ready; });

describe("multi-viewer dynamic — withdraw / disconnect", () => {
  test("3A — withdrawing the pinning viewer grows SCD to remaining min", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 200, 60);
    mgr.claimViewport(CID, FP_B, 80, 24);
    expect(readApplied(mgr)).toEqual({ cols: 80, rows: 24 });
    mgr.withdrawViewport(CID, FP_B);
    await afterWithdraw();
    expect(readApplied(mgr)).toEqual({ cols: 200, rows: 60 });
  });

  test("3B — withdrawing non-pinning viewer → SCD unchanged", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 80, 24);
    mgr.claimViewport(CID, FP_B, 200, 60);
    mgr.withdrawViewport(CID, FP_B);
    expect(readApplied(mgr)).toEqual({ cols: 80, rows: 24 });
  });

  test("3C — withdrawing last viewer keeps lastAppliedSize (no thrash)", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 100, 30);
    expect(readApplied(mgr)).toEqual({ cols: 100, rows: 30 });
    mgr.withdrawViewport(CID, FP_A);
    expect(readApplied(mgr)).toEqual({ cols: 100, rows: 30 });
  });

  test("3D — cols=0 (or rows=0) is treated as withdraw (deferred)", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 200, 60);
    mgr.claimViewport(CID, FP_B, 100, 30);
    mgr.claimViewport(CID, FP_B, 0, 30);    // cols=0 = withdraw B
    await afterWithdraw();
    expect(readApplied(mgr)).toEqual({ cols: 200, rows: 60 });
    mgr.claimViewport(CID, FP_A, 150, 0);   // rows=0 = withdraw A
    await afterWithdraw();
    const claims = (mgr as unknown as {
      viewportClaims: Map<number, Map<string, unknown>>;
    }).viewportClaims.get(CID)!;
    expect(claims.size).toBe(0);
  });

  test("3E — withdraw of unknown fp → silent no-op (no recompute, no throw)", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 100, 30);
    expect(() => mgr.withdrawViewport(CID, FP_B)).not.toThrow();
    expect(readApplied(mgr)).toEqual({ cols: 100, rows: 30 });
  });
});

describe("multi-viewer dynamic — TTL reaper", () => {
  test("6A — claim older than VIEWPORT_CLAIM_TTL_MS gets reaped → SCD recomputes", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 200, 60);
    mgr.claimViewport(CID, FP_B, 60, 20);   // B was pinning
    expect(readApplied(mgr)).toEqual({ cols: 60, rows: 20 });
    ageClaim(mgr, FP_B, 121_000);            // B goes quiet, exceeds TTL
    reapNow(mgr);
    expect(readApplied(mgr)).toEqual({ cols: 200, rows: 60 });
  });

  test("6B — claim refreshed within TTL → reaper leaves it alone", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 200, 60);
    mgr.claimViewport(CID, FP_B, 100, 30);   // fresh
    reapNow(mgr);
    expect(readApplied(mgr)).toEqual({ cols: 100, rows: 30 });
  });

  test("6C — claim aged 60s (half-TTL) is NOT dropped", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 200, 60);
    mgr.claimViewport(CID, FP_B, 100, 30);
    ageClaim(mgr, FP_B, 60_000);
    reapNow(mgr);
    expect(readApplied(mgr)).toEqual({ cols: 100, rows: 30 });
  });

  test("6E — stale-seq re-claim does NOT overwrite dims (reorder guard)", async () => {
    // clientSeq is no longer a latest-pointer (SCD min is order-independent)
    // — it survives ONLY as a reorder guard: a re-claim whose seq has not
    // advanced (heartbeat / WAN reorder) refreshes liveness but must NOT
    // overwrite the viewer's dims, so a stale packet can't regress the SCD.
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 200, 60, 1);
    mgr.claimViewport(CID, FP_B, 60, 20, 1);    // B pins the min → 60×20
    expect(readApplied(mgr)).toEqual({ cols: 60, rows: 20 });
    // B re-sends with the SAME seq but bogus dims (stale heartbeat) →
    // dims ignored → SCD unchanged.
    mgr.claimViewport(CID, FP_B, 999, 999, 1);
    expect(readApplied(mgr)).toEqual({ cols: 60, rows: 20 });
    // B genuinely resizes with an ADVANCED seq → dims update → recompute.
    mgr.claimViewport(CID, FP_B, 200, 60, 2);
    expect(readApplied(mgr)).toEqual({ cols: 200, rows: 60 }); // min(200,200)
  });

  test("6F — #1 WAN reorder: late packet with stale seq is ignored for latest", async () => {
    // Two intent-bearing claims from A: seq=5 (large window), seq=6
    // (small window after resize). If the seq=5 packet is delayed and
    // arrives AFTER seq=6, worker must not regress latest back to
    // seq=5's dims.
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 200, 60, 5);   // intent seq=5
    mgr.claimViewport(CID, FP_A, 100, 30, 6);   // intent seq=6 — arrives second
    expect(readApplied(mgr)).toEqual({ cols: 100, rows: 30 });
    // Reorder: late seq=5 packet arrives now. lastMs refreshes, but
    // seq has not advanced → no latest bump and dims stay at seq=6.
    mgr.claimViewport(CID, FP_A, 200, 60, 5);
    expect(readApplied(mgr)).toEqual({ cols: 100, rows: 30 });
  });

  test("6D — withdraw recomputes SCD = min(remaining), order/recency-independent", async () => {
    // SCD has no latest-pointer and no lastMs tiebreak: after the smallest
    // viewer leaves, the size is purely min(remaining) regardless of
    // insertion order or which claim was refreshed most recently.
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    // Insertion order A, C, B; refresh A last so lastMs would (wrongly)
    // favor it under any recency scheme — SCD must ignore that.
    mgr.claimViewport(CID, FP_A, 200, 60);
    mgr.claimViewport(CID, FP_C, 100, 30);
    ageClaim(mgr, FP_A, 5_000);
    ageClaim(mgr, FP_C, 4_000);
    mgr.claimViewport(CID, FP_A, 200, 60);   // A refreshed last (freshest lastMs)
    mgr.claimViewport(CID, FP_B, 60, 20);    // B smallest → SCD 60×20
    expect(readApplied(mgr)).toEqual({ cols: 60, rows: 20 });
    mgr.withdrawViewport(CID, FP_B);          // smallest leaves
    await afterWithdraw();
    // min(A=200×60, C=100×30) = 100×30 — NOT A despite its fresh lastMs.
    expect(readApplied(mgr)).toEqual({ cols: 100, rows: 30 });
  });
});
