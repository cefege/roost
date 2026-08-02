// Dead-birth self-heal: when the keeper reports a child exited within
// DEAD_BIRTH_LIFETIME_MS of spawn having emitted ZERO bytes (head_seq===0),
// that's a stillborn PTY — the degraded-survivor-keeper class. N within the
// window → onKeeperDegraded fires (main.ts grace-gates the actual keeper
// restart). head_seq>0 (real output) or a slow exit must NOT count, so a
// legit fast-exiting shell can't trip it. Drives SessionManager.closedByKeeper
// directly with injected records; the fsm is stubbed (we assert the self-heal
// callback, not the FSM transition).

import { describe, test, expect } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared";
import { initCellEmitState } from "@roost/shared/cell";
import { createSbRing } from "../src/session-scrollback-ring.ts";

function freshMgr(): { mgr: SessionManager; calls: { n: number } } {
  const calls = { n: 0 };
  const mgr = new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
  });
  mgr.setOnKeeperDegraded(() => { calls.n++; });
  return { mgr, calls };
}

// Inject a SessionRecord the dead-birth check reads: head_seq + spawnedAtMs.
// fsm is a stub (closedByKeeper calls fsm.send; we don't assert on it here).
function injectSession(mgr: SessionManager, channelId: number, headSeq: number, ageMs: number): void {
  const record = {
    sessionId: asSessionId(`00000000-0000-0000-0000-${String(channelId).padStart(12, "0")}`),
    channelId: asChannelId(channelId),
    socketPath: `mux:${channelId}`,
    kind: "shell" as const,
    cwd: "/",
    fsm: { send: () => {} } as never,
    bridge: null,
    scrollback: createSbRing(),
    head_seq: headSeq,
    alt_mode: false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    wtermCore: null as never,
    session_trace_id: "t",
    cell_emit: initCellEmitState(),
    spawnedAtMs: Date.now() - ageMs,
  };
  (mgr as unknown as { sessions: Map<number, unknown> }).sessions.set(channelId, record);
}

describe("keeper dead-birth self-heal", () => {
  test("3 zero-byte instant exits → onKeeperDegraded fires once at the threshold", () => {
    const { mgr, calls } = freshMgr();
    for (let ch = 1; ch <= 2; ch++) { injectSession(mgr, ch, 0, 10); mgr.closedByKeeper(ch, 0); }
    expect(calls.n).toBe(0); // below threshold (3)
    injectSession(mgr, 3, 0, 10); mgr.closedByKeeper(3, 0);
    expect(calls.n).toBe(1); // threshold reached
  });

  test("a child that produced output (head_seq>0) is NOT a dead-birth", () => {
    const { mgr, calls } = freshMgr();
    for (let ch = 1; ch <= 5; ch++) { injectSession(mgr, ch, 42, 10); mgr.closedByKeeper(ch, 0); }
    expect(calls.n).toBe(0);
  });

  test("a slow exit (lifetime ≥ 2s) is NOT a dead-birth even with zero bytes", () => {
    const { mgr, calls } = freshMgr();
    for (let ch = 1; ch <= 5; ch++) { injectSession(mgr, ch, 0, 5_000); mgr.closedByKeeper(ch, 0); }
    expect(calls.n).toBe(0);
  });

  test("burst resets after firing so the post-restart respawn can't re-trigger mid-grace", () => {
    const { mgr, calls } = freshMgr();
    for (let ch = 1; ch <= 3; ch++) { injectSession(mgr, ch, 0, 10); mgr.closedByKeeper(ch, 0); }
    expect(calls.n).toBe(1);
    // Two more dead-births: below the (reset) threshold → no second fire yet.
    for (let ch = 4; ch <= 5; ch++) { injectSession(mgr, ch, 0, 10); mgr.closedByKeeper(ch, 0); }
    expect(calls.n).toBe(1);
  });
});
