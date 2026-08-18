// Multi-viewer dynamic invariants. Two-or-more browser tabs/computers
// connected to the same channel, each with its own (cols,rows), each
// resizing/disconnecting/refreshing independently. Every test here
// drives SessionManager directly via claimViewport/withdrawViewport,
// then asserts the SCD that the worker applied to (a) the PTY pool
// (silent no-op against a null mux socket — acceptable; we read the
// lastAppliedSize cache instead) and (b) the headless wterm-core.
//
// Sibling file multi-viewer-scrollback.test.ts pins the per-fetch
// snapshot invariants (I1-I3). This file pins the CLAIM-side math:
// dynamic resize, disconnect, reaper, claim refresh, N-viewer SCD.
//
// Shared setup for the multi-viewer-dynamic-*.test.ts siblings: imports,
// constants, and reflection helpers. Not a *.test.ts file, so bun-test
// won't run it standalone.

import { SessionManager } from "../src/session-manager.ts";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared";
import { VIEWER_WITHDRAW_GRACE_MS } from "@roost/shared/viewport";
import { WasmBridge } from "@wterm/core";
import { initCellEmitState } from "@roost/shared/cell";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";
import { installAutoKeeper } from "./keeper-fake-pool.ts";
import { settleTerminalControl } from "./terminal-control-settle.ts";

// withdrawViewport defers removal by VIEWER_WITHDRAW_GRACE_MS (hysteresis,
// so a refresh's re-claim cancels it). Tests that withdraw then assert the
// recomputed SCD must wait past the grace for the deferred removal to land.
export const afterWithdraw = () => {
  const { promise, resolve } = Promise.withResolvers<void>();
  setTimeout(resolve, VIEWER_WITHDRAW_GRACE_MS + 150);
  return promise;
};

const SID = asSessionId("00000000-0000-0000-0000-000000000000");
export const CID = 1;

export const FP_A = "a".repeat(64);
export const FP_B = "b".repeat(64);
export const FP_C = "c".repeat(64);
// A resize is only APPLIED once the keeper acknowledges it, so these fixtures
// need a keeper that answers: without one every resize is a truthful pre-write
// rejection and lastAppliedSize would never advance. installAutoKeeper models the
// real keeper's sequence idempotence; the caller restores the pool in dispose.
export function freshMgr(initCols = 80, initRows = 24): SessionManager {
  installAutoKeeper({ cols: initCols, rows: initRows });
  return new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
  });
}

export async function injectSession(mgr: SessionManager, initCols: number, initRows: number): Promise<void> {
  const wtermCore = await WasmBridge.load();
  wtermCore.init(initCols, initRows);
  const record = {
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
    query_carry: new Uint8Array(0),
    ...initAgentOscState(),
    wtermCore,
    cell_emit: initCellEmitState("test-grid"),
  };
  (mgr as unknown as { sessions: Map<number, unknown> }).sessions.set(CID, record);
  // spawnShell records the size the keeper created the PTY at; an injected
  // session must too, or the first claim at that same size looks like a resize.
  mgr.lastAppliedSize.set(CID, { cols: initCols, rows: initRows });
}

// Read the SCD size the worker PROVED with the keeper. Claims are transactional
// now (claim → terminal-control lane → keeper resize → proven size), so a read
// must drain that lane; a synchronous read would observe the size from before the
// claim. The helper owns the drain so no test can forget it.
export async function readApplied(mgr: SessionManager): Promise<{ cols: number; rows: number } | undefined> {
  await settleTerminalControl(mgr, CID);
  return mgr.lastAppliedSize.get(CID);
}

// The wtermCore size — second source of truth, kept in sync by the same
// transaction that proved the PTY size.
export async function readWtermSize(mgr: SessionManager): Promise<{ cols: number; rows: number }> {
  await settleTerminalControl(mgr, CID);
  const core = mgr.sessions.get(CID)!.wtermCore;
  return { cols: core.getCols(), rows: core.getRows() };
}

// Re-exported so the sibling scenario files share one drain implementation.
export { settleTerminalControl } from "./terminal-control-settle.ts";

// Age an existing claim's lastMs in place. Models a real browser that
// claimed live once but then went quiet (lost connectivity, OS sleep).
// Going through claimViewport-then-mutate (rather than synthesizing
// the claim from scratch) preserves the lastAppliedSize side-effect
// that the reaper relies on to detect "claim was the pinning one".
export function ageClaim(mgr: SessionManager, fp: string, ageMs: number): void {
  const claim = mgr.viewportClaims.get(CID)?.get(fp);
  if (!claim) throw new Error(`ageClaim: no live claim for ${fp}`);
  claim.lastMs = Date.now() - ageMs;
}

export function reapNow(mgr: SessionManager): void {
  mgr._reapViewportClaims();
}

export function recomputeNow(mgr: SessionManager): void {
  mgr.reconcileTerminalViewport(CID);
}

export function claimExists(mgr: SessionManager, fp: string): boolean {
  return !!mgr.viewportClaims.get(CID)?.has(fp);
}

export const ready = (async () => { await WasmBridge.load(); })();
