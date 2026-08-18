// Super-functional hardening (2026-06-22b): a DEGRADED survivor keeper births
// dead PTYs — it emits on channels the worker no longer maps (emit_no_session).
// Today keeper DEATH auto-heals (setOnKeeperDeath → reconcile); degradation only
// logged a signal. This wires degradation to a grace-gated keeper restart so it
// self-heals instead of leaving sessions in "can't input" until a manual kick.
// This test pins: sustained emit_no_session → onKeeperDegraded callback fires.

import { describe, test, expect } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { asWorkerFp } from "@roost/shared";
import { initCellEmitState } from "@roost/shared/cell";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";

function mgr(): { mgr: SessionManager; degradedCalls: number } {
  const state = { degradedCalls: 0 };
  const m = new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
    sendBinaryUpstream: () => {}, // present so emitUpstreamChunk reaches appendScrollback
  });
  m.setOnKeeperDegraded(() => { state.degradedCalls++; });
  return { mgr: m, get degradedCalls() { return state.degradedCalls; } } as never;
}

// Drive PtyOut on a channel that has NO session record → appendScrollback
// returns -1 → emit_no_session. KEEPER_DEGRADED_THRESHOLD (5) within the window
// promotes to keeper.degraded + the self-heal callback.
const emitOrphan = (m: SessionManager, ch: number): void =>
  (m as unknown as { emitUpstreamChunk(c: number, b: Buffer): void }).emitUpstreamChunk(ch, Buffer.from("x"));

// Mark a channel as just-closed (sets recentlyClosed) — drives the post-close
// tail path in emitUpstreamChunk.
const dropChannel = (m: SessionManager, ch: number): void =>
  (m as unknown as { _dropChannelState(c: number): void })._dropChannelState(ch);

describe("keeper degradation self-heal", () => {
  test("sustained emit_no_session fires onKeeperDegraded", () => {
    const h = mgr();
    // 4 orphan emits — below threshold, no fire yet.
    for (let i = 0; i < 4; i++) emitOrphan(h.mgr, 999);
    expect(h.degradedCalls).toBe(0);
    // 5th crosses KEEPER_DEGRADED_THRESHOLD → fire.
    emitOrphan(h.mgr, 999);
    expect(h.degradedCalls).toBeGreaterThanOrEqual(1);
  });

  test("a mapped channel (normal output) never fires degradation", async () => {
    const { WasmBridge } = await import("@wterm/core");
    const h = mgr();
    const core = await WasmBridge.load(); core.init(80, 24);
    (h.mgr as unknown as { sessions: Map<number, unknown> }).sessions.set(7, {
      sessionId: "00000000-0000-0000-0000-000000000000", channelId: 7, socketPath: "/dev/null",
      kind: "shell", cwd: "/", fsm: {}, bridge: null, scrollback: createSbRing(),
      head_seq: 0, alt_mode: false, mode_carry: new Uint8Array(0), osc7_carry: new Uint8Array(0), query_carry: new Uint8Array(0),
      ...initAgentOscState(),
      wtermCore: core, cell_emit: initCellEmitState("test-grid"),
    });
    for (let i = 0; i < 10; i++) emitOrphan(h.mgr, 7); // ch 7 IS mapped → no emit_no_session
    expect(h.degradedCalls).toBe(0);
  });

  // Restart-loop fix (2026-06-23): a just-closed channel emits a few tail bytes
  // (prompt epilogue / exit msg) AFTER teardown — benign. These must NOT count
  // toward degradation or they re-trip keeper.degraded after every reconcile →
  // SIGTERM-storm restart loop. See project_keeper_degradation_dead_birth_selfheal.
  test("post-close tail emits within TTL do NOT fire degradation", () => {
    const h = mgr();
    dropChannel(h.mgr, 55);              // mark recentlyClosed
    for (let i = 0; i < 10; i++) emitOrphan(h.mgr, 55); // tail bytes, within 750ms TTL
    expect(h.degradedCalls).toBe(0);     // silently dropped, no _noSessionBurst
  });

  test("orphan emits PAST the close TTL still fire degradation (true orphan)", async () => {
    const h = mgr();
    dropChannel(h.mgr, 56);
    await new Promise((r) => setTimeout(r, 800)); // past RECENTLY_CLOSED_TTL_MS (750)
    for (let i = 0; i < 6; i++) emitOrphan(h.mgr, 56); // now counts — keeper truly driving a dead channel
    expect(h.degradedCalls).toBeGreaterThanOrEqual(1);
  });
});
