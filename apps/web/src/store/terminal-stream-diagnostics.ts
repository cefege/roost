import { currentSyncV2TerminalState } from "./sync.ts";
import {
  activeTerminalResyncView,
  terminalGenerationMatches,
  terminalGenerationToken,
} from "./terminal-stream-replica.ts";
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
  TerminalStreamDiagnosticSnapshot,
} from "./terminal-stream-types.ts";
function generationSnapshot(
  token: TerminalGenerationToken | null | undefined,
): TerminalGenerationDiagnosticToken | null {
  if (!token) return null;
  return {
    socketGeneration: token.socketGeneration,
    socketId: token.socketId,
    processEpoch: token.processEpoch,
    domainGeneration: token.domainGeneration.toString(),
  };
}

function monotonicAge(now: number, startedAt: number | null | undefined): number | null {
  return startedAt === null || startedAt === undefined
    ? null
    : Math.max(0, now - startedAt);
}

function smokeFaultsEnabled(): boolean {
  if (import.meta.env.VITE_ROOST_SMOKE !== "1") return false;
  try {
    return localStorage.getItem("roostSmoke") === "1";
  } catch {
    return false;
  }
}

export function blackholeTerminalFramesForCurrentGeneration(sessionId: string): void {
  if (!smokeFaultsEnabled()) return;
  const sync = currentSyncV2TerminalState();
  if (!sync?.ready) return;
  terminalBlackholeFaults.set(sessionId, {
    generation: terminalGenerationToken(sync),
  });
  terminalBlackholeDropCounts.set(sessionId, 0);
}

export function dropNextTerminalWireDelta(sessionId: string): void {
  if (!smokeFaultsEnabled()) return;
  const sync = currentSyncV2TerminalState();
  if (!sync?.ready) return;
  terminalWireDeltaFaults.set(sessionId, {
    generation: terminalGenerationToken(sync),
  });
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
): TerminalStreamDiagnosticSnapshot {
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

/** Tear down every dashboard-bound replica before a new dashboard can dial.
 * A smoke-only one-shot renderer-loss arm survives this replay boundary so its
 * first returned baseline still exercises recovery. */
export function resetTerminalStream(): void {
  resetTerminalStreamState(true);
}

export function _resetTerminalStreamForTest(): void {
  resetTerminalStreamState();
}
