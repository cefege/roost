import { currentSyncV2TerminalState } from "./sync.ts";
import { activeTerminalResyncView } from "./terminal-stream-replica.ts";
import {
  pruneTerminalSessionState,
  persistTerminalRendererDrop,
  resetTerminalStreamState,
  terminalDropNextFrames,
  terminalDroppedFrameCounts,
  terminalFrameCounts,
  terminalFullFrameCounts,
  terminalFullFrameScrollbackRows,
  terminalGridEpochs,
  terminalSessions,
} from "./terminal-stream-state.ts";
import type { TerminalStreamDiagnosticSnapshot } from "./terminal-stream-types.ts";

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
    },
    replica: {
      expected_stream_id: session?.expectedStreamId ?? null,
      grid_epoch: session?.canonical?.gridEpoch ?? null,
      seq: session?.canonical?.seq ?? null,
      baseline_ready: session?.baselineReady ?? false,
      resync_latched: session?.resyncLatched ?? false,
    },
    wire_received: {
      stream_id: session?.wireStreamId ?? null,
      grid_epoch: session?.wireGridEpoch ?? null,
      seq: session?.wireSeq ?? null,
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

export function _resetTerminalStreamForTest(): void {
  resetTerminalStreamState();
}
