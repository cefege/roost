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
export function freshMgr(): SessionManager {
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
    scrollback: new Uint8Array(0),
    head_seq: 0,
    alt_mode: false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    wtermCore,
    cell_emit: initCellEmitState(),
  };
  (mgr as unknown as { sessions: Map<number, unknown> }).sessions.set(CID, record);
}

// Read the SIGWINCH-cached size. Surface is private; tests use the same
// reflection trick as session-manager-seqno.test.ts.
export function readApplied(mgr: SessionManager): { cols: number; rows: number } | undefined {
  return (mgr as unknown as {
    lastAppliedSize: Map<number, { cols: number; rows: number }>;
  }).lastAppliedSize.get(CID);
}

// Read the wtermCore size — second source of truth that
// _recomputeViewport keeps in sync with the PTY.
export function readWtermSize(mgr: SessionManager): { cols: number; rows: number } {
  const rec = (mgr as unknown as { sessions: Map<number, { wtermCore: { getCols(): number; getRows(): number } }> }).sessions.get(CID)!;
  return { cols: rec.wtermCore.getCols(), rows: rec.wtermCore.getRows() };
}

// OPT2-1: _recomputeViewport now applies the SCD to the wtermCore via an
// async rebuild-from-ring (the PTY SIGWINCH is still synchronous). Await the
// per-channel rebuild chain before reading wtermCore dims.
export async function flushRebuild(mgr: SessionManager): Promise<void> {
  await (mgr as unknown as { _wtermRebuildChain: Map<number, Promise<void>> })
    ._wtermRebuildChain.get(CID);
}

// Age an existing claim's lastMs in place. Models a real browser that
// claimed live once but then went quiet (lost connectivity, OS sleep).
// Going through claimViewport-then-mutate (rather than synthesizing
// the claim from scratch) preserves the lastAppliedSize side-effect
// that the reaper relies on to detect "claim was the pinning one".
export function ageClaim(mgr: SessionManager, fp: string, ageMs: number): void {
  const claim = (mgr as unknown as {
    viewportClaims: Map<number, Map<string, { lastMs: number }>>;
  }).viewportClaims.get(CID)?.get(fp);
  if (!claim) throw new Error(`ageClaim: no live claim for ${fp}`);
  claim.lastMs = Date.now() - ageMs;
}

export function reapNow(mgr: SessionManager): void {
  (mgr as unknown as { _reapViewportClaims(): void })._reapViewportClaims();
}

export function recomputeNow(mgr: SessionManager): void {
  (mgr as unknown as { _recomputeViewport(c: number): void })._recomputeViewport(CID);
}

export function claimExists(mgr: SessionManager, fp: string): boolean {
  return !!(mgr as unknown as {
    viewportClaims: Map<number, Map<string, unknown>>;
  }).viewportClaims.get(CID)?.has(fp);
}

export const ready = (async () => { await WasmBridge.load(); })();
