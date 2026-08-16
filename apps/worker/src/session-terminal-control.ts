import { asChannelId } from "@roost/shared";
import type { SessionManager } from "./session-manager.ts";
import type { ViewportClaim } from "./session-record.ts";
import { desiredViewportSize, needsClaimSnapshot } from "./session-viewport.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";

const BACKGROUND_CAUSE = 5;
const SNAPSHOT_CAUSES = new Set([1, 3, 6]);
const RETRY_DELAY_MS = 25;

export type WorkerInputResult =
  | { status: "accepted"; writtenBytes: number }
  | { status: "rejected"; writtenBytes: 0; reason: string }
  | { status: "ambiguous"; writtenBytes: number; reason: string };

export type WorkerViewportResult =
  | { status: "committed"; channelResizeSeq: number; cols: number; rows: number; resized: boolean }
  | { status: "rejected"; reason: string };

export interface WorkerViewportIntent {
  sessionId: string;
  viewerId: string;
  clientSeq: bigint;
  cols: number;
  rows: number;
  cause: number;
  heldCellSeq: bigint;
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export async function writeTerminalInput(
  this: SessionManager,
  sessionId: string,
  inputSeq: bigint,
  bytes: Uint8Array,
): Promise<WorkerInputResult> {
  const rec = this.getBySessionId(sessionId);
  if (!rec) return { status: "rejected", writtenBytes: 0, reason: "session is not live" };
  if (bytes.byteLength === 0) return { status: "accepted", writtenBytes: 0 };
  if (inputSeq <= 0n || inputSeq > BigInt(Number.MAX_SAFE_INTEGER)) {
    return { status: "rejected", writtenBytes: 0, reason: "input sequence exceeds keeper protocol range" };
  }
  const pool = getMultiplexedPool();
  if (!pool.socket) return { status: "rejected", writtenBytes: 0, reason: "keeper is disconnected" };
  const owned = bytes.slice();
  (this as SessionManager & { markInputSensitive?: (channelId: number) => void })
    .markInputSensitive?.(rec.channelId);
  try {
    const result = await pool.requestInput(rec.channelId, Number(inputSeq), owned);
    if (result.kind === "ack") {
      return result.writtenBytes === owned.byteLength
        ? { status: "accepted", writtenBytes: result.writtenBytes }
        : { status: "ambiguous", writtenBytes: result.writtenBytes, reason: "keeper acknowledged an incomplete input batch" };
    }
    if (result.kind === "reject") {
      return { status: "rejected", writtenBytes: 0, reason: result.reason };
    }
    return { status: "ambiguous", writtenBytes: result.writtenBytes ?? 0, reason: result.reason };
  } catch (error) {
    return { status: "ambiguous", writtenBytes: 0, reason: error instanceof Error ? error.message : String(error) };
  }
}

function restoreViewerClaim(
  mgr: SessionManager,
  channelId: number,
  viewerId: string,
  installed: ViewportClaim | null,
  prior: ViewportClaim | undefined,
): void {
  const claims = mgr.viewportClaims.get(channelId);
  if (!claims) return;
  if (installed === null) {
    if (claims.has(viewerId)) return;
  } else if (claims.get(viewerId) !== installed) return;
  if (prior) claims.set(viewerId, prior);
  else claims.delete(viewerId);
}

function drainPostResizeOutput(mgr: SessionManager, channelId: number): void {
  const queued = mgr.postResizeOutput.get(channelId);
  mgr.postResizeOutput.delete(channelId);
  if (!queued) return;
  for (const chunk of queued) mgr.emitUpstreamChunk(channelId, chunk);
}

async function nextResizeSequence(mgr: SessionManager, channelId: number): Promise<number> {
  const known = mgr.channelResizeSeq.get(channelId);
  if (known !== undefined) return known + 1;
  const history = await getMultiplexedPool().getHistoryRecords(channelId);
  let latest = 0;
  for (const record of history.records) {
    if (record.kind === "resize") latest = Math.max(latest, record.seq);
  }
  mgr.channelResizeSeq.set(channelId, latest);
  return latest + 1;
}

async function commitResize(
  mgr: SessionManager,
  channelId: number,
  resizeSeq: number,
  cols: number,
  rows: number,
): Promise<{ seq: number; cols: number; rows: number }> {
  const pool = getMultiplexedPool();
  let result = await pool.requestResize(channelId, resizeSeq, cols, rows);
  while (result.kind === "unknown") {
    if (!mgr.sessions.has(channelId)) throw new Error("session closed while resize was unresolved");
    result = await pool.queryResizeStatus(channelId, resizeSeq);
    if (result.kind === "ack" && result.seq < resizeSeq) {
      await delay(RETRY_DELAY_MS);
      result = await pool.requestResize(channelId, resizeSeq, cols, rows);
    }
  }
  if (result.kind === "reject") throw new Error(result.reason);
  if (result.seq !== resizeSeq || result.cols !== cols || result.rows !== rows) {
    throw new Error("keeper returned a conflicting resize status");
  }
  return result;
}

async function applyViewportNow(
  mgr: SessionManager,
  channelId: number,
  intent: WorkerViewportIntent,
): Promise<WorkerViewportResult> {
  const rec = mgr.sessions.get(channelId);
  if (!rec || rec.sessionId !== intent.sessionId) return { status: "rejected", reason: "session is not live" };
  let claims = mgr.viewportClaims.get(channelId);
  if (!claims) {
    claims = new Map();
    mgr.viewportClaims.set(channelId, claims);
  }
  const prior = claims.get(intent.viewerId);
  const priorSeq = prior?.clientSeq ?? -1n;
  const isBackground = intent.cause === BACKGROUND_CAUSE;
  const withdraw = !isBackground && (intent.cols <= 0 || intent.rows <= 0);
  if (intent.clientSeq < priorSeq) return { status: "rejected", reason: "stale viewport sequence" };
  if (intent.clientSeq === priorSeq) {
    const equivalent = withdraw ? prior === undefined : prior?.cols === intent.cols && prior.rows === intent.rows;
    if (!equivalent) return { status: "rejected", reason: "conflicting viewport sequence" };
    if (prior) prior.lastMs = Date.now();
    if (SNAPSHOT_CAUSES.has(intent.cause)
      && needsClaimSnapshot(mgr, channelId, Number(intent.heldCellSeq), claims.size > 0)) {
      mgr.emitCellSnapshot(asChannelId(channelId));
    }
    const size = mgr.lastAppliedSize.get(channelId) ?? { cols: rec.wtermCore.getCols(), rows: rec.wtermCore.getRows() };
    return { status: "committed", channelResizeSeq: mgr.channelResizeSeq.get(channelId) ?? 0, cols: size.cols, rows: size.rows, resized: false };
  }

  const wasStreaming = claims.size > 0;
  const shouldSnapshot = !withdraw && needsClaimSnapshot(mgr, channelId, Number(intent.heldCellSeq), wasStreaming);
  const installed: ViewportClaim | null = withdraw ? null : {
    cols: intent.cols,
    rows: intent.rows,
    lastMs: Date.now(),
    clientSeq: intent.clientSeq,
  };
  mgr._cancelPendingWithdraw(channelId, intent.viewerId);
  if (installed) claims.set(intent.viewerId, installed);
  else claims.delete(intent.viewerId);

  const desired = desiredViewportSize.call(mgr, channelId);
  const current = mgr.lastAppliedSize.get(channelId) ?? { cols: rec.wtermCore.getCols(), rows: rec.wtermCore.getRows() };
  if (!desired || (desired.cols === current.cols && desired.rows === current.rows)) {
    if (shouldSnapshot) mgr.emitCellSnapshot(asChannelId(channelId));
    return { status: "committed", channelResizeSeq: mgr.channelResizeSeq.get(channelId) ?? 0, cols: current.cols, rows: current.rows, resized: false };
  }

  const resizeSeq = await nextResizeSequence(mgr, channelId);
  mgr.cellEmissionGates.add(channelId);
  try {
    const result = await commitResize(mgr, channelId, resizeSeq, desired.cols, desired.rows);
    // Keeper dispatch yields before post-ACK PtyOut. Buffer synchronously at the
    // continuation, rebuild at the boundary, then parse bytes at new geometry.
    mgr.postResizeOutput.set(channelId, []);
    await mgr._rebuildWtermCore(channelId, result.cols, result.rows);
    mgr.lastAppliedSize.set(channelId, { cols: result.cols, rows: result.rows });
    mgr.channelResizeSeq.set(channelId, result.seq);
    drainPostResizeOutput(mgr, channelId);
    mgr.cellEmissionGates.delete(channelId);
    mgr.emitCellSnapshot(asChannelId(channelId));
    return { status: "committed", channelResizeSeq: result.seq, cols: result.cols, rows: result.rows, resized: true };
  } catch (error) {
    restoreViewerClaim(mgr, channelId, intent.viewerId, installed, prior);
    mgr.postResizeOutput.delete(channelId);
    mgr.cellEmissionGates.delete(channelId);
    if (mgr.sessions.has(channelId)) mgr.emitCellSnapshot(asChannelId(channelId));
    return { status: "rejected", reason: error instanceof Error ? error.message : String(error) };
  }
}

export function applyTerminalViewport(
  this: SessionManager,
  intent: WorkerViewportIntent,
): Promise<WorkerViewportResult> {
  const rec = this.getBySessionId(intent.sessionId);
  if (!rec) return Promise.resolve({ status: "rejected", reason: "session is not live" });
  const channelId = rec.channelId;
  const prior = this.terminalControlChains.get(channelId) ?? Promise.resolve();
  const result = prior.catch(() => undefined).then(() => applyViewportNow(this, channelId, intent));
  const tail = result.then(() => undefined, () => undefined);
  this.terminalControlChains.set(channelId, tail);
  void tail.finally(() => {
    if (this.terminalControlChains.get(channelId) === tail) this.terminalControlChains.delete(channelId);
  });
  return result;
}
