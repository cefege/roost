import { describe, test, expect, beforeAll } from "bun:test";
import {
  CID,
  FP_A,
  FP_B,
  FP_C,
  freshMgr,
  injectSession,
  readApplied,
  readWtermSize,
  flushRebuild,
  ageClaim,
  recomputeNow,
  claimExists,
  ready,
} from "./multi-viewer-dynamic-helpers.ts";

beforeAll(async () => { await ready; });

describe("A3 — liveness-weighted SCD (stale claim stops clipping before TTL)", () => {
  test("dead small viewer is excluded from SCD-min ~70s before the 120s reap", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 40, 20);   // tiny phone
    mgr.claimViewport(CID, FP_B, 120, 40);  // desktop
    expect(readApplied(mgr)).toEqual({ cols: 40, rows: 20 }); // SCD min while both fresh

    // Phone dies ungracefully (no withdraw). Age its claim past the fresh
    // cutoff (70s) but NOT past the 120s TTL — so it's still in the map.
    ageClaim(mgr, FP_A, 80_000);
    recomputeNow(mgr);

    expect(readApplied(mgr)).toEqual({ cols: 120, rows: 40 }); // desktop no longer clipped
    expect(claimExists(mgr, FP_A)).toBe(true); // excluded from min, NOT yet removed
  });

  test("all-stale claims → PTY left as-is (no thrash), reaper still owns removal", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 100, 30);
    expect(readApplied(mgr)).toEqual({ cols: 100, rows: 30 });
    ageClaim(mgr, FP_A, 80_000);
    recomputeNow(mgr);
    // No fresh claim → size unchanged (don't resize a running TUI to a default).
    expect(readApplied(mgr)).toEqual({ cols: 100, rows: 30 });
  });
});

describe("multi-viewer dynamic — SCD math", () => {
  test("1A — single viewer claim → SCD = that viewer's dims", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 120, 40);
    expect(readApplied(mgr)).toEqual({ cols: 120, rows: 40 });
    await flushRebuild(mgr);
    expect(readWtermSize(mgr)).toEqual({ cols: 120, rows: 40 });
  });

  test("1B — two viewers, B smaller → SCD = B", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 120, 40);
    mgr.claimViewport(CID, FP_B, 100, 30);
    expect(readApplied(mgr)).toEqual({ cols: 100, rows: 30 });
  });

  test("1C — two viewers equal → SCD = either", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 100, 30);
    mgr.claimViewport(CID, FP_B, 100, 30);
    expect(readApplied(mgr)).toEqual({ cols: 100, rows: 30 });
  });

  test("1D — three viewers → SCD = min across all", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 200, 60);
    mgr.claimViewport(CID, FP_B, 120, 40);
    mgr.claimViewport(CID, FP_C, 90, 28);
    expect(readApplied(mgr)).toEqual({ cols: 90, rows: 28 });
  });

  test("1E — claim refresh same fp replaces (doesn't double-count)", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 80, 24);
    mgr.claimViewport(CID, FP_A, 200, 60);
    // Only A claims; SCD = its latest.
    expect(readApplied(mgr)).toEqual({ cols: 200, rows: 60 });
    const claims = (mgr as unknown as {
      viewportClaims: Map<number, Map<string, unknown>>;
    }).viewportClaims.get(CID)!;
    expect(claims.size).toBe(1);
  });

  test("1F — SCD takes per-dim min: narrow+tall ∩ wide+short = narrow+short", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 80, 60);   // narrow + tall
    mgr.claimViewport(CID, FP_B, 200, 24);  // wide + short
    // Each dimension min'd independently → the intersection both can render.
    expect(readApplied(mgr)).toEqual({ cols: 80, rows: 24 });
  });
});

describe("multi-viewer dynamic — resize over time", () => {
  test("2A — SCD = min regardless of claim order/recency", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 80, 24);
    mgr.claimViewport(CID, FP_B, 100, 30);  // B larger; A still pins the min
    expect(readApplied(mgr)).toEqual({ cols: 80, rows: 24 });
    mgr.claimViewport(CID, FP_B, 200, 60);  // B grows further — A still pins
    expect(readApplied(mgr)).toEqual({ cols: 80, rows: 24 });
    mgr.claimViewport(CID, FP_A, 90, 28);   // A grows above prior min
    expect(readApplied(mgr)).toEqual({ cols: 90, rows: 28 }); // min(90,200)×min(28,60)
  });

  test("2B — pinning viewer resizes smaller → SCD shrinks", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 100, 30);
    mgr.claimViewport(CID, FP_B, 80, 24);   // B pinning
    expect(readApplied(mgr)).toEqual({ cols: 80, rows: 24 });
    mgr.claimViewport(CID, FP_B, 60, 20);   // B shrinks further
    expect(readApplied(mgr)).toEqual({ cols: 60, rows: 20 });
  });

  test("2C — sole viewer resizes → SCD follows exactly", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 80, 24);
    mgr.claimViewport(CID, FP_A, 160, 50);
    mgr.claimViewport(CID, FP_A, 100, 32);
    expect(readApplied(mgr)).toEqual({ cols: 100, rows: 32 });
    await flushRebuild(mgr);
    expect(readWtermSize(mgr)).toEqual({ cols: 100, rows: 32 });
  });

  test("2D — claim refresh at same dims → lastAppliedSize stable, no thrash", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, FP_A, 100, 30);
    const beforeRef = readApplied(mgr);
    mgr.claimViewport(CID, FP_A, 100, 30);
    mgr.claimViewport(CID, FP_A, 100, 30);
    const afterRef = readApplied(mgr);
    expect(afterRef).toEqual(beforeRef!);
  });

  test("2E — rapid resize sequence converges to the final claim", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    for (let c = 200; c >= 60; c -= 10) mgr.claimViewport(CID, FP_A, c, c >> 1);
    expect(readApplied(mgr)).toEqual({ cols: 60, rows: 30 });
  });
});
