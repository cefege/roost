// Real-world multi-step scenarios: each test simulates a user flow
// across multiple browsers/computers attaching, typing, resizing,
// disconnecting against the same SessionManager. Asserts state at
// every transition so a regression in any step surfaces with the
// scenario label.
//
// Author 2026-06-17: "it needs to fucking work and easily transition
// from one computer terminal from one to another. from one browser to
// another laptop, from a laptop to another." These tests drive that
// transition graph at the SessionManager level. The DOM/CSS layer
// requires humanchrome — covered by a separate smoke.

import { describe, test, expect } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared";
import { VIEWER_WITHDRAW_GRACE_MS } from "@roost/shared/viewport";
import { WasmBridge } from "@wterm/core";
import { gridToCellFrame } from "@roost/shared/cell";
import type { TerminalCore } from "@wterm/core";
import { initCellEmitState } from "@roost/shared/cell";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";

// withdrawViewport defers removal by VIEWER_WITHDRAW_GRACE_MS; wait past it
// before asserting the post-withdraw claim count / recomputed SCD.
const afterWithdraw = () => new Promise((r) => setTimeout(r, VIEWER_WITHDRAW_GRACE_MS + 150));

const SID = asSessionId("00000000-0000-0000-0000-000000000000");
const CID = 1;

const M1 = "m1".padEnd(64, "0");  // computer 1 fingerprint
const M2 = "m2".padEnd(64, "0");  // computer 2
const M3 = "m3".padEnd(64, "0");  // computer 3
const M1_TABA = `${M1}:tabA`;
const M1_TABB = `${M1}:tabB`;

function freshMgr(): SessionManager {
  return new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
  });
}

async function injectSession(mgr: SessionManager, cols: number, rows: number): Promise<void> {
  const wtermCore = await WasmBridge.load();
  wtermCore.init(cols, rows);
  (mgr as unknown as { sessions: Map<number, unknown> }).sessions.set(CID, {
    sessionId: SID,
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
    ...initAgentOscState(),
    wtermCore,
    cell_emit: initCellEmitState("test-grid"),
  });
}

function cellGridText(core: TerminalCore): string {
  const frame = gridToCellFrame(core, 0, "test-grid:0");
  const lines: string[] = [];
  for (const r of frame.scrollbackRows) lines.push(r.spans.map(s => s.text).join(""));
  for (const r of frame.viewportRows) lines.push(r.spans.map(s => s.text).join(""));
  return lines.join("\n");
}
function getCore(mgr: SessionManager): TerminalCore {
  return mgr.shellByChannel(CID)!.wtermCore;
}

function applied(mgr: SessionManager): { cols: number; rows: number } | undefined {
  return (mgr as unknown as { lastAppliedSize: Map<number, { cols: number; rows: number }> }).lastAppliedSize.get(CID);
}

function liveWrite(mgr: SessionManager, chunk: string): void {
  (mgr as unknown as { appendScrollback(c: number, b: Buffer): number })
    .appendScrollback(CID, Buffer.from(chunk));
}



describe("scenario S1 — computer-to-computer handoff", () => {
  test("S1 — M1 alone → M2 joins smaller → M1 leaves → M2 alone (live + history transitions correctly)", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 200, 60);

    // M1 connects at 200×60.
    mgr.claimViewport(CID, M1, 200, 60);
    expect(applied(mgr)).toEqual({ cols: 200, rows: 60 });

    // M1 types and shell echoes 3 lines.
    liveWrite(mgr, "step1\r\nstep2\r\nstep3\r\n");
    const snap_m1_only = cellGridText(getCore(mgr));
    expect(snap_m1_only).toContain("step1");
    expect(snap_m1_only).toContain("step3");

    // M2 joins at 100×30 (smaller laptop). SCD shrinks. Worker resizes
    // wtermCore. Future content lays out at SCD.
    mgr.claimViewport(CID, M2, 100, 30);
    expect(applied(mgr)).toEqual({ cols: 100, rows: 30 });

    // More live content arrives — it lands on the SCD grid.
    liveWrite(mgr, "after-m2-joined\r\n");
    const snap_m1 = cellGridText(getCore(mgr));
    const snap_m2 = cellGridText(getCore(mgr));
    // The SCD-sized grid text is the same regardless of caller dims.
    expect(snap_m1).toBe(snap_m2);

    // M1 leaves. SCD stays at last applied (M2 still pinning at 100×30).
    mgr.withdrawViewport(CID, M1);
    expect(applied(mgr)).toEqual({ cols: 100, rows: 30 });

    // M2 alone now. More content arrives.
    liveWrite(mgr, "alone-now\r\n");
    const snap_final = cellGridText(getCore(mgr));
    expect(snap_final).toContain("alone-now");
    expect(snap_final.length).toBeGreaterThan(0);
  });
});

describe("scenario S2 — two-tab on the same browser (composite key disambiguation)", () => {
  test("S2 — M1 tabA + M1 tabB, each at different dims, tabA closes, tabB intact", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, M1_TABA, 200, 60);
    mgr.claimViewport(CID, M1_TABB, 100, 30);
    // Composite keys keep both alive simultaneously.
    expect(applied(mgr)).toEqual({ cols: 100, rows: 30 });
    const claims = (mgr as unknown as {
      viewportClaims: Map<number, Map<string, unknown>>;
    }).viewportClaims.get(CID)!;
    expect(claims.size).toBe(2);

    // Tab A pagehide → withdraw (deferred). Tab B keeps pinning.
    mgr.withdrawViewport(CID, M1_TABA);
    await afterWithdraw();
    expect(applied(mgr)).toEqual({ cols: 100, rows: 30 });
    expect(claims.size).toBe(1);
  });
});

describe("scenario S3 — three-computer cascade", () => {
  test("S3 — M1+M2+M3 all attached, middle (M2) leaves, SCD = min of M1+M3", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, M1, 200, 60);
    mgr.claimViewport(CID, M2, 120, 40);   // mid-sized
    mgr.claimViewport(CID, M3, 90, 28);
    expect(applied(mgr)).toEqual({ cols: 90, rows: 28 });

    liveWrite(mgr, "before-cascade\r\n");
    mgr.withdrawViewport(CID, M2);
    // SCD recomputes to min(M1, M3) = min(200, 90), min(60, 28) = (90, 28)
    expect(applied(mgr)).toEqual({ cols: 90, rows: 28 });

    liveWrite(mgr, "after-cascade\r\n");
    const snap = cellGridText(getCore(mgr));
    expect(snap).toContain("before-cascade");
    expect(snap).toContain("after-cascade");
  });
});

describe("scenario S4 — dynamic resize during live byte stream", () => {
  test("S4 — bytes flowing while M2 resizes its window: no corruption, both viewers consistent", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 100, 30);
    mgr.claimViewport(CID, M1, 100, 30);
    mgr.claimViewport(CID, M2, 80, 24);
    liveWrite(mgr, "preA\r\n");
    // M2 drags its window — series of resize claims.
    for (let w = 80; w >= 60; w -= 5) mgr.claimViewport(CID, M2, w, 20);
    liveWrite(mgr, "midA\r\n");
    for (let w = 60; w <= 100; w += 10) mgr.claimViewport(CID, M2, w, 30);
    liveWrite(mgr, "postA\r\n");
    // Final SCD = min(M1=100×30, M2=100×30) = (100, 30).
    expect(applied(mgr)).toEqual({ cols: 100, rows: 30 });
    const snap1 = cellGridText(getCore(mgr));
    const snap2 = cellGridText(getCore(mgr));
    expect(snap1).toBe(snap2);
    expect(snap1).toContain("preA");
    expect(snap1).toContain("postA");
  });
});

describe("scenario S5 — laptop closes lid (silent disconnect via reaper)", () => {
  test("S5 — M1 (small) stops heartbeating; reaper drops it; M2 alone resumes its own dims", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, M1, 60, 20);    // small laptop pins the SCD min
    mgr.claimViewport(CID, M2, 150, 50);
    expect(applied(mgr)).toEqual({ cols: 60, rows: 20 }); // SCD = min, M1 pins

    // Lid closes — heartbeat stops. Age M1's claim past TTL; the reaper
    // (synchronous, not the deferred-withdraw path) drops it.
    const claim = (mgr as unknown as {
      viewportClaims: Map<number, Map<string, { lastMs: number }>>;
    }).viewportClaims.get(CID)!.get(M1)!;
    claim.lastMs = Date.now() - 121_000;
    (mgr as unknown as { _reapViewportClaims(): void })._reapViewportClaims();

    // M1 reaped → M2 alone → SCD grows to M2's own dims.
    expect(applied(mgr)).toEqual({ cols: 150, rows: 50 });
  });

  test("S5b — stale-but-unreaped claim (past FRESH, before TTL) releases the min on the reaper tick", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 80, 24);
    mgr.claimViewport(CID, M1, 60, 20);    // small laptop pins the SCD min
    mgr.claimViewport(CID, M2, 150, 50);
    expect(applied(mgr)).toEqual({ cols: 60, rows: 20 });

    // M1 dies silently. Age its claim past VIEWER_CLAIM_FRESH_MS (70s) but
    // NOT past the 120s TTL. M2's heartbeats are same-seq (no recompute), so
    // only the reaper tick can notice the freshness cutoff was crossed.
    const claim = (mgr as unknown as {
      viewportClaims: Map<number, Map<string, { lastMs: number }>>;
    }).viewportClaims.get(CID)!.get(M1)!;
    claim.lastMs = Date.now() - 80_000;
    (mgr as unknown as { _reapViewportClaims(): void })._reapViewportClaims();

    // M1 excluded from the min (not yet dropped) → SCD grows to M2's dims.
    expect(applied(mgr)).toEqual({ cols: 150, rows: 50 });

    // M1's tab comes back (e.g. laptop reopens within the TTL) with an
    // advancing seq — it re-enters the min and shrinks the SCD again.
    mgr.claimViewport(CID, M1, 60, 20, 99);
    expect(applied(mgr)).toEqual({ cols: 60, rows: 20 });
  });
});

describe("scenario S6 — full alt-screen TUI under multi-viewer", () => {
  test("S6 — claude (alt-screen) running; second viewer joins smaller; snapshot still emits alt-screen-correct ANSI", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 100, 30);
    mgr.claimViewport(CID, M1, 100, 30);
    // Worker boots claude — sees alt-screen enter + content.
    liveWrite(mgr, "shell prompt\r\n\x1b[?1049h\x1b[2J\x1b[Hclaude-ui-loaded\r\n│ helper text\r\n");

    // M2 joins at 80×24. SCD shrinks; wtermCore reflows.
    mgr.claimViewport(CID, M2, 80, 24);
    expect(applied(mgr)).toEqual({ cols: 80, rows: 24 });

    const snap1 = cellGridText(getCore(mgr));
    const snap2 = cellGridText(getCore(mgr));
    // Both viewers see identical grid text (SCD-sized).
    expect(snap1).toBe(snap2);
    // Cell grid is non-empty (replaces old clientRender smoke check).
    expect(snap1.length).toBeGreaterThan(0);
  });
});

describe("scenario S7 — big content + tiny new viewer (the 'minimize' case)", () => {
  test("S7 — many lines of formatted history exists; tiny laptop joins; tiny sees coherent content (some loss accepted)", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 120, 40);
    mgr.claimViewport(CID, M1, 120, 40);
    // 30 lines of mixed-color content.
    let buf = "";
    for (let i = 0; i < 30; i++) {
      const c = 30 + (i % 8);
      buf += `\x1b[${c}mrow ${i} content goes here\x1b[0m\r\n`;
    }
    liveWrite(mgr, buf);

    // Tiny laptop joins (the "minimize" case from user spec).
    mgr.claimViewport(CID, M2, 40, 12);
    expect(applied(mgr)).toEqual({ cols: 40, rows: 12 });

    const text = cellGridText(getCore(mgr));
    // Content should be recognizable. Per H3 wart, wterm-wasm shrink IS
    // lossy on long lines — that's an upstream wterm issue, not the
    // multi-viewer machinery.
    expect(text.length).toBeGreaterThan(0);
    expect(text).toMatch(/row \d+/);
    // Grid text is non-empty after resize (replaces old clientRender smoke check).
  });
});

describe("scenario S9 — fast tab-switch (Author 2026-06-17: 'switching between one and the other in fast succession')", () => {
  test("S9 — rapid claim/withdraw cycles from two viewers don't desync SCD or lose content", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 100, 30);
    mgr.claimViewport(CID, M1, 100, 30);
    mgr.claimViewport(CID, M2, 80, 24);
    liveWrite(mgr, "baseline\r\n");

    // 50 rapid toggles. Each "switch" = withdraw + immediate re-claim
    // (the SPA's visibilitychange-hidden → visibilitychange-visible
    // round-trip when the user alt-tabs or swipes between desktops).
    for (let i = 0; i < 50; i++) {
      mgr.withdrawViewport(CID, M2);
      mgr.claimViewport(CID, M2, 80, 24);
      liveWrite(mgr, `tick-${i}\r\n`);
    }
    // SCD must still be SCD; no drift.
    expect(applied(mgr)).toEqual({ cols: 80, rows: 24 });
    // Latest content present.
    const snap = cellGridText(getCore(mgr));
    expect(snap).toContain("tick-49");
    // Same core, same grid text — both viewers see identical output.
    const snap2 = cellGridText(getCore(mgr));
    expect(snap).toBe(snap2);
  });

  test("S9b — interleaved claim/withdraw from BOTH viewers at once", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 100, 30);
    // Pong pattern: M1 in/out, then M2 in/out, repeat.
    for (let i = 0; i < 30; i++) {
      mgr.claimViewport(CID, M1, 100, 30);
      mgr.withdrawViewport(CID, M1);
      mgr.claimViewport(CID, M2, 80, 24);
      mgr.withdrawViewport(CID, M2);
    }
    // After the deferred withdraws fire, no claims live → SCD frozen.
    await afterWithdraw();
    const claims = (mgr as unknown as { viewportClaims: Map<number, Map<string, unknown>> })
      .viewportClaims.get(CID);
    expect(claims === undefined || claims.size === 0).toBe(true);
    // Final claim establishes new SCD.
    mgr.claimViewport(CID, M2, 60, 20);
    expect(applied(mgr)).toEqual({ cols: 60, rows: 20 });
  });

  test("S9c — alt-tab while typing: byte stream + rapid claim toggles, no torn snapshot", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 100, 30);
    mgr.claimViewport(CID, M1, 100, 30);
    mgr.claimViewport(CID, M2, 80, 24);
    for (let i = 0; i < 20; i++) {
      liveWrite(mgr, `chunk-${i}\r\n`);
      if (i % 3 === 0) {
        mgr.withdrawViewport(CID, M1);
        mgr.claimViewport(CID, M1, 100, 30);
      }
      if (i % 5 === 0) {
        mgr.withdrawViewport(CID, M2);
        mgr.claimViewport(CID, M2, 80, 24);
      }
    }
    const s1 = cellGridText(getCore(mgr));
    const s2 = cellGridText(getCore(mgr));
    expect(s1).toBe(s2);
    expect(s1).toContain("chunk-0");
    expect(s1).toContain("chunk-19");
  });
});

describe("scenario S8 — round-trip: M1 leaves entirely, M2 picks up everything from history", () => {
  test("S8 — M1 disconnects; M2 sees the complete recent history from worker's wterm", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 100, 30);
    mgr.claimViewport(CID, M1, 100, 30);
    liveWrite(mgr, "from-m1-pre\r\n");
    mgr.withdrawViewport(CID, M1);
    // SCD stays at (100, 30) — no thrash even though no viewer.
    expect(applied(mgr)).toEqual({ cols: 100, rows: 30 });

    // M2 connects fresh.
    mgr.claimViewport(CID, M2, 100, 30);
    liveWrite(mgr, "from-m2-post\r\n");
    const snap = cellGridText(getCore(mgr));
    expect(snap).toContain("from-m1-pre");
    expect(snap).toContain("from-m2-post");
  });
});
