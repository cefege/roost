// Terminal stream diagnostics — snapshots only elected transport state for smoke
// and incident readers. The replica owns frame truth; the direct registry owns
// route election, so a staged candidate is reported separately from the carrier.

import { terminalPeerAttemptSnapshot, type TerminalPeerAttemptSnapshot } from "../ws/terminal-peer.ts";
import { currentSyncV2TerminalState, type SyncV2TerminalState } from "./sync.ts";
import {
  activeTerminalResyncView,
  terminalGenerationMatches,
  terminalGenerationToken,
} from "./terminal-stream-replica.ts";
import { currentTerminalGenerationToken } from "./terminal-stream-publication.ts";
import { terminalSessionPromotionConnection } from "./terminal-stream-promotion.ts";
import {
  terminalDirectRegistry,
  type TerminalDirectConnection,
  type TerminalDirectConnectionTelemetry,
  type TerminalPeerCandidateType,
} from "./terminal-stream-transport.ts";
import { terminalInputPendingSnapshot, terminalInputPhase } from "../ws/terminal-input-router.ts";
import { syncTerminalWorkerControlTelemetry } from "../ws/sync-terminal-control-probe.ts";
import {
  pruneTerminalSessionState,
  persistTerminalRendererDrop,
  resetTerminalStreamState,
  terminalBlackholeDropCounts,
  terminalBlackholeFaults,
  terminalDropNextFrames,
  terminalDroppedFrameCounts,
  terminalFrameCounts,
  terminalFullFrameCounts,
  terminalFullFrameScrollbackRows,
  terminalGridEpochs,
  terminalWireDeltaDropCounts,
  terminalWireDeltaFaults,
  terminalWireDeltaDroppedSeq,
  terminalWireDeltaPostDropSeq,
  terminalSessions,
} from "./terminal-stream-state.ts";
import type {
  TerminalGenerationDiagnosticToken,
  TerminalGenerationToken,
  TerminalStreamDiagnosticSnapshot as TerminalStreamDiagnosticSnapshotBase,
  TerminalTransportKind,
} from "./terminal-stream-types.ts";

type TerminalPeerPhase = TerminalPeerAttemptSnapshot["phase"];
type TerminalPeerFallbackReason = TerminalPeerAttemptSnapshot["fallbackReason"];

export interface TerminalRouteDiagnosticEntry {
  kind: TerminalTransportKind;
  worker_epoch: string | null;
  peer_id: string | null;
  phase: "active" | "candidate";
  candidate_type: TerminalPeerCandidateType;
  probe_age_ms: number | null;
  rtt_ms: number | null;
  /** Content-free control round-trip to the selected worker, not a browser ping. */
  worker_control_rtt_ms: number | null;
  buffered_bytes: number | null;
}

export interface TerminalRouteDiagnosticSnapshot {
  active: TerminalRouteDiagnosticEntry | null;
  candidate: TerminalRouteDiagnosticEntry | null;
  peer_phase: TerminalPeerPhase | null;
  fallback_reason: TerminalPeerFallbackReason;
  failure_detail: string | null;
  input_phase: import("../ws/terminal-input-route-claim.ts").TerminalInputPhase | null;
  pending_input_count: number;
}

export type TerminalStreamDiagnosticsSnapshot = TerminalStreamDiagnosticSnapshotBase & {
  route: TerminalRouteDiagnosticSnapshot;
};

function generationSnapshot(
  token: TerminalGenerationToken | null | undefined,
): TerminalGenerationDiagnosticToken | null {
  if (!token) return null;
  return {
    socketGeneration: token.socketGeneration,
    socketId: token.socketId,
    processEpoch: token.processEpoch,
    domainGeneration: token.domainGeneration.toString(),
    transportKind: token.transportKind,
    workerFp: token.workerFp,
  };
}

function monotonicAge(now: number, startedAt: number | null | undefined): number | null {
  return typeof startedAt === "number" && Number.isFinite(startedAt)
    ? Math.max(0, now - startedAt)
    : null;
}

function directTelemetry(
  connection: TerminalDirectConnection,
): TerminalDirectConnectionTelemetry | null {
  try {
    return connection.telemetry?.() ?? null;
  } catch {
    return null;
  }
}

function directRouteEntry(
  connection: TerminalDirectConnection,
  phase: "active" | "candidate",
  now: number,
): TerminalRouteDiagnosticEntry {
  const telemetry = directTelemetry(connection);
  const rttMs = telemetry?.rttMs;
  const controlRttMs = typeof rttMs === "number" && Number.isFinite(rttMs) && rttMs >= 0
    ? rttMs
    : null;
  const bufferedBytes = telemetry?.bufferedBytes;
  return {
    kind: connection.kind,
    worker_epoch: connection.kind === "loopback" && connection.workerEpoch === connection.connectionId
      ? null
      : connection.workerEpoch || null,
    peer_id: telemetry?.opaquePeerId ?? null,
    phase,
    candidate_type: telemetry?.candidateType ?? "none",
    probe_age_ms: monotonicAge(now, telemetry?.lastProbeAtMs),
    rtt_ms: controlRttMs,
    worker_control_rtt_ms: controlRttMs,
    buffered_bytes: typeof bufferedBytes === "number"
      && Number.isFinite(bufferedBytes)
      && bufferedBytes >= 0
      ? bufferedBytes
      : null,
  };
}

function syncRouteEntry(
  workerFp: string | undefined,
  sync: SyncV2TerminalState | null,
  now: number,
): TerminalRouteDiagnosticEntry {
  const telemetry = workerFp ? syncTerminalWorkerControlTelemetry(workerFp, sync) : null;
  return {
    kind: "sync",
    worker_epoch: telemetry?.workerEpoch ?? null,
    peer_id: null,
    phase: "active",
    candidate_type: "none",
    probe_age_ms: telemetry ? monotonicAge(now, telemetry.lastProbeAtMs) : null,
    rtt_ms: telemetry?.controlRttMs ?? null,
    worker_control_rtt_ms: telemetry?.controlRttMs ?? null,
    buffered_bytes: null,
  };
}

function terminalRouteSnapshot(
  sessionId: string,
  workerFp: string | undefined,
  now: number,
  sync: SyncV2TerminalState | null,
): TerminalRouteDiagnosticSnapshot {
  const registeredActive = terminalDirectRegistry.activeForSession(sessionId);
  const activeDirect = registeredActive?.workerFp === workerFp
    ? registeredActive
    : null;
  const activeToken = currentTerminalGenerationToken(sessionId, sync);
  const stagingConnection = terminalSessionPromotionConnection(sessionId);
  const candidate = stagingConnection ?? (workerFp
    ? terminalDirectRegistry.candidateForWorker(workerFp)
    : null);
  const peer = workerFp ? terminalPeerAttemptSnapshot(workerFp) : null;
  return {
    active: activeDirect
      ? directRouteEntry(activeDirect, "active", now)
      : activeToken?.transportKind === "sync"
      ? syncRouteEntry(workerFp, sync, now)
      : null,
    candidate: candidate
      && candidate.workerFp === workerFp
      && (stagingConnection !== null || candidate !== activeDirect)
      && candidate.allowsSession(sessionId)
      ? directRouteEntry(candidate, "candidate", now)
      : null,
    peer_phase: peer?.phase ?? null,
    fallback_reason: peer?.fallbackReason ?? null,
    failure_detail: peer?.lastFailureDetail ?? null,
    input_phase: terminalInputPhase(sessionId),
    pending_input_count: terminalInputPendingSnapshot(sessionId).count,
  };
}

function smokeFaultsEnabled(): boolean {
  if (import.meta.env.VITE_ROOST_SMOKE !== "1") return false;
  try {
    return localStorage.getItem("roostSmoke") === "1";
  } catch {
    return false;
  }
}

function currentTerminalSmokeGeneration(sessionId: string): TerminalGenerationToken | null {
  const sync = currentSyncV2TerminalState();
  const generation = currentTerminalGenerationToken(sessionId, sync);
  if (!generation) return null;
  return generation.transportKind === "sync" && !sync?.ready
    ? null
    : generation;
}
export function blackholeTerminalFramesForCurrentGeneration(sessionId: string): void {
  if (!smokeFaultsEnabled()) return;
  const generation = currentTerminalSmokeGeneration(sessionId);
  if (!generation) return;
  terminalBlackholeFaults.set(sessionId, { generation });
  terminalBlackholeDropCounts.set(sessionId, 0);
}

export function dropNextTerminalWireDelta(sessionId: string): void {
  if (!smokeFaultsEnabled()) return;
  const generation = currentTerminalSmokeGeneration(sessionId);
  if (!generation) return;
  terminalWireDeltaFaults.set(sessionId, { generation });
  terminalWireDeltaDropCounts.set(sessionId, 0);
  terminalWireDeltaDroppedSeq.delete(sessionId);
  terminalWireDeltaPostDropSeq.delete(sessionId);
}

export function consumeTerminalSmokeFrameFault(
  sessionId: string,
  owner: TerminalGenerationToken,
  kind: "frame" | "chunk",
  full: boolean,
  seq: number | null,
): boolean {
  const blackhole = terminalBlackholeFaults.get(sessionId);
  if (blackhole) {
    if (!terminalGenerationMatches(blackhole.generation, owner)) {
      terminalBlackholeFaults.delete(sessionId);
    } else {
      terminalBlackholeDropCounts.set(
        sessionId,
        (terminalBlackholeDropCounts.get(sessionId) ?? 0) + 1,
      );
      return true;
    }
  }
  if (kind !== "frame" || full) return false;
  if (
    seq !== null
    && terminalWireDeltaDroppedSeq.has(sessionId)
    && !terminalWireDeltaPostDropSeq.has(sessionId)
  ) {
    terminalWireDeltaPostDropSeq.set(sessionId, seq);
  }
  const wireDrop = terminalWireDeltaFaults.get(sessionId);
  if (!wireDrop) return false;
  if (!terminalGenerationMatches(wireDrop.generation, owner)) {
    terminalWireDeltaFaults.delete(sessionId);
    return false;
  }
  terminalWireDeltaFaults.delete(sessionId);
  terminalWireDeltaDropCounts.set(
    sessionId,
    (terminalWireDeltaDropCounts.get(sessionId) ?? 0) + 1,
  );
  if (seq !== null) terminalWireDeltaDroppedSeq.set(sessionId, seq);
  return true;
}

export function terminalStreamDiagnosticSnapshot(
  sessionId: string,
  preferredViewId?: string,
): TerminalStreamDiagnosticsSnapshot {
  const session = terminalSessions.get(sessionId);
  const sync = currentSyncV2TerminalState();
  const preferred = preferredViewId
    ? session?.handles.get(preferredViewId)
    : undefined;
  const view = preferred ?? (session ? activeTerminalResyncView(session) : null)
    ?? session?.handles.values().next().value
    ?? null;
  const status = view?.status ?? null;
  const statusStream = status && "streamId" in status ? status.streamId : null;
  const statusCols = status && "effectiveCols" in status ? status.effectiveCols : null;
  const statusRows = status && "effectiveRows" in status ? status.effectiveRows : null;
  const now = performance.now();
  return {
    view: {
      view_id: view?.viewId ?? null,
      revision: view?.desired?.revision.toString() ?? null,
      active: view?.desired?.active ?? false,
      status: status?.status ?? null,
      stream_id: statusStream,
      effective_cols: statusCols,
      effective_rows: statusRows,
      lease_deadline_ms: view?.leaseDeadlineMs ?? null,
      pending_ack_age_ms: monotonicAge(now, view?.pendingViewAckAtMs),
      pending_ack_generation: generationSnapshot(view?.pendingViewAckGeneration),
    },
    replica: {
      expected_stream_id: session?.expectedStreamId ?? null,
      grid_epoch: session?.canonical?.gridEpoch ?? null,
      seq: session?.canonical?.seq ?? null,
      baseline_ready: session?.baselineReady ?? false,
      resync_latched: session?.resyncLatched ?? false,
      last_terminal_proof_age_ms: monotonicAge(now, session?.lastAcceptedFrameAtMs),
      last_terminal_proof_generation: generationSnapshot(
        session?.lastAcceptedFrameGeneration,
      ),
      challenge_age_ms: monotonicAge(now, session?.proofChallengeAtMs),
      challenge_generation: generationSnapshot(session?.proofChallengeGeneration),
      challenge_stream_id: session?.proofChallengeStreamId ?? null,
      challenge_seq: session?.proofChallengeSeq ?? null,
      resync_latch_age_ms: monotonicAge(now, session?.resyncLatchedAtMs),
      resync_latch_generation: generationSnapshot(session?.resyncLatchGeneration),
      repair_attempts: session?.repairAttempts ?? 0,
      repair_outcome: session?.repairOutcome ?? "none",
    },
    wire_received: {
      stream_id: session?.wireStreamId ?? null,
      grid_epoch: session?.wireGridEpoch ?? null,
      seq: session?.wireSeq ?? null,
    },
    faults: {
      blackhole_drop_count: terminalBlackholeDropCounts.get(sessionId) ?? 0,
      wire_delta_drop_count: terminalWireDeltaDropCounts.get(sessionId) ?? 0,
      wire_delta_dropped_seq: terminalWireDeltaDroppedSeq.get(sessionId) ?? null,
      wire_delta_post_drop_seq: terminalWireDeltaPostDropSeq.get(sessionId) ?? null,
    },
    sync: {
      socket_generation: sync?.socketGeneration ?? null,
      socket_id: sync?.socketId ?? null,
      process_epoch: sync?.processEpoch ?? null,
      domain_generation: sync?.domainGeneration.toString() ?? null,
      ready: sync?.ready ?? false,
    },
    route: terminalRouteSnapshot(sessionId, session?.workerFp, now, sync),
  };
}

export function pruneTerminalSession(sessionId: string): void {
  pruneTerminalSessionState(sessionId);
}

export function cellFrameCount(sessionId: string): number {
  return terminalFrameCounts.get(sessionId) ?? 0;
}

export function cellFullFrameCount(sessionId: string): number {
  return terminalFullFrameCounts.get(sessionId) ?? 0;
}

export function cellFrameCountSize(): number {
  return terminalFrameCounts.size;
}

export function lastFullFrameSbRows(sessionId: string): number {
  return terminalFullFrameScrollbackRows.get(sessionId) ?? -1;
}

export function cellGridEpoch(sessionId: string): string {
  return terminalGridEpochs.get(sessionId) ?? "";
}

export function dropNextCellFrame(sessionId: string): void {
  // Build-time gate first: prod bundles fold the whole drop path away.
  if (import.meta.env.VITE_ROOST_SMOKE !== "1") return;
  try {
    if (localStorage.getItem("roostSmoke") !== "1") return;
  } catch {
    return;
  }
  terminalDropNextFrames.add(sessionId);
  persistTerminalRendererDrop(sessionId);
}

export function droppedCellFrameCount(sessionId: string): number {
  return terminalDroppedFrameCounts.get(sessionId) ?? 0;
}

/** Tear down every credential-bound replica before another socket can dial.
 * A smoke-only one-shot renderer-loss arm survives this replay boundary so its
 * first returned baseline still exercises recovery. */
export function resetTerminalStream(): void {
  resetTerminalStreamState(true);
}

export function _resetTerminalStreamForTest(): void {
  resetTerminalStreamState();
}
