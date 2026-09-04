import { CellGridChunkAssembler } from "@roost/shared/cell";
import type {
  TerminalGenerationToken,
  TerminalSessionReplica,
  TerminalViewHandleStatus,
  TerminalViewRecord,
} from "./terminal-stream-types.ts";

/** Singleton browser ownership for every live per-session replica and its diagnostics. */
export const terminalSessions = new Map<string, TerminalSessionReplica>();
export const terminalFrameCounts = new Map<string, number>();
export const terminalFullFrameCounts = new Map<string, number>();
export const terminalFullFrameScrollbackRows = new Map<string, number>();
export const terminalGridEpochs = new Map<string, string>();
export const terminalDropNextFrames = new Set<string>();
export const terminalDroppedFrameCounts = new Map<string, number>();
export interface TerminalGenerationFaultRecord {
  generation: TerminalGenerationToken;
}
export const terminalBlackholeFaults = new Map<string, TerminalGenerationFaultRecord>();
export const terminalWireDeltaFaults = new Map<string, TerminalGenerationFaultRecord>();
export const terminalBlackholeDropCounts = new Map<string, number>();
export const terminalWireDeltaDropCounts = new Map<string, number>();
export const terminalWireDeltaDroppedSeq = new Map<string, number>();
export const terminalWireDeltaPostDropSeq = new Map<string, number>();

// Drop keys are written only from the smoke-gated dropNextCellFrame path, so
// deriving the prefix from the build flag drops the roostSmoke literal from
// prod bundles while smoke-build keys stay byte-identical.
const TERMINAL_DROP_STORAGE_PREFIX =
  import.meta.env.VITE_ROOST_SMOKE === "1" ? "roostSmoke.dropCell." : "";

function terminalDropStorageKey(sessionId: string): string {
  return `${TERMINAL_DROP_STORAGE_PREFIX}${sessionId}`;
}

export function persistTerminalRendererDrop(sessionId: string): void {
  try {
    sessionStorage.setItem(terminalDropStorageKey(sessionId), "1");
  } catch {
    // The in-memory arm still covers pages where storage is unavailable.
  }
}

export function takePersistedTerminalRendererDrop(sessionId: string): boolean {
  try {
    const key = terminalDropStorageKey(sessionId);
    if (sessionStorage.getItem(key) !== "1") return false;
    sessionStorage.removeItem(key);
    return true;
  } catch {
    return false;
  }
}

function clearPersistedTerminalRendererDrops(): void {
  try {
    for (let index = sessionStorage.length - 1; index >= 0; index--) {
      const key = sessionStorage.key(index);
      if (key?.startsWith(TERMINAL_DROP_STORAGE_PREFIX)) {
        sessionStorage.removeItem(key);
      }
    }
  } catch {
    // Storage cleanup is best-effort in privacy-restricted contexts.
  }
}

export const terminalGenerationObservation: {
  key: string | null;
  initialized: boolean;
} = {
  key: null,
  initialized: false,
};

export function terminalSessionReplica(sessionId: string): TerminalSessionReplica {
  let session = terminalSessions.get(sessionId);
  if (session) return session;
  session = {
    sessionId,
    handles: new Map(),
    subscribers: new Set(),
    expectedStreamId: null,
    effectiveCols: 0,
    effectiveRows: 0,
    canonical: null,
    baselineReady: false,
    requiresFreshBaseline: true,
    resyncLatched: false,
    resyncSentGeneration: null,
    resyncRetryGeneration: null,
    resyncRetryAtMs: null,
    generation: null,
    idleProbeTimer: null,
    proofDeadlineTimer: null,
    lastAcceptedFrameAtMs: null,
    lastAcceptedFrameGeneration: null,
    proofChallengeAtMs: null,
    proofChallengeGeneration: null,
    resyncLatchedAtMs: null,
    resyncLatchGeneration: null,
    repairAttempts: 0,
    repairOutcome: "none",
    assembler: new CellGridChunkAssembler(),
    chunkTimer: null,
    wireStreamId: null,
    wireGridEpoch: null,
    wireSeq: null,
  };
  terminalSessions.set(sessionId, session);
  return session;
}

export function emitTerminalViewStatus(
  view: TerminalViewRecord,
  status: TerminalViewHandleStatus,
): void {
  view.status = status;
  for (const listener of view.statusListeners) listener(status);
}

function discardTerminalSessionState(session: TerminalSessionReplica): void {
  session.assembler.reset();
  clearTimeout(session.chunkTimer ?? undefined);
  session.chunkTimer = null;
  // A dashboard reset may tear down the replica before Solid unmounts the
  // CellTerminal that owns these handles. Mark every handle inert first: its
  // later cleanup must never publish an inactive view against the next
  // dashboard's Sync generation.
  session.generation = null;
  session.lastAcceptedFrameAtMs = null;
  session.lastAcceptedFrameGeneration = null;
  clearTimeout(session.idleProbeTimer ?? undefined);
  session.idleProbeTimer = null;
  clearTimeout(session.proofDeadlineTimer ?? undefined);
  session.proofDeadlineTimer = null;
  session.proofChallengeAtMs = null;
  session.proofChallengeGeneration = null;
  session.resyncLatchedAtMs = null;
  session.resyncLatchGeneration = null;
  session.repairOutcome = "pruned";
  for (const view of session.handles.values()) {
    view.disposed = true;
    clearInterval(view.heartbeat ?? undefined);
    view.heartbeat = null;
    clearTimeout(view.viewAckTimer ?? undefined);
    view.viewAckTimer = null;
    view.pendingViewAckAtMs = null;
    view.pendingViewAckGeneration = null;
    view.pendingViewAckRevision = null;
    clearInterval(view.progressTimer ?? undefined);
    view.progressTimer = null;
    view.statusListeners.clear();
    view.progressListeners.clear();
    view.rendererSubscribers.clear();
  }
  session.handles.clear();
  session.subscribers.clear();
}

export function pruneTerminalSessionState(sessionId: string): void {
  const session = terminalSessions.get(sessionId);
  if (session) {
    discardTerminalSessionState(session);
    terminalSessions.delete(sessionId);
  }
  if (import.meta.env.VITE_ROOST_SMOKE === "1") {
    terminalBlackholeFaults.delete(sessionId);
    terminalWireDeltaFaults.delete(sessionId);
    terminalBlackholeDropCounts.delete(sessionId);
    terminalWireDeltaDropCounts.delete(sessionId);
    terminalWireDeltaDroppedSeq.delete(sessionId);
    terminalWireDeltaPostDropSeq.delete(sessionId);
  }
  terminalFrameCounts.delete(sessionId);
  terminalFullFrameCounts.delete(sessionId);
  terminalFullFrameScrollbackRows.delete(sessionId);
  terminalGridEpochs.delete(sessionId);
  terminalDropNextFrames.delete(sessionId);
  terminalDroppedFrameCounts.delete(sessionId);
}

export function resetTerminalStreamState(preservePendingRendererDrops = false): void {
  // A smoke renderer-loss arm names one session and is consumed exactly once.
  // Keep that deliberate delivery seam across a dashboard reset/replay so a
  // reset between arming and the first returned full frame cannot erase it.
  // Ordinary test teardown still clears every arm.
  const pendingRendererDrops = preservePendingRendererDrops
    ? new Set(terminalDropNextFrames)
    : null;
  for (const sessionId of Array.from(terminalSessions.keys())) {
    pruneTerminalSessionState(sessionId);
  }
  terminalFrameCounts.clear();
  terminalFullFrameCounts.clear();
  terminalFullFrameScrollbackRows.clear();
  terminalGridEpochs.clear();
  if (!preservePendingRendererDrops) clearPersistedTerminalRendererDrops();
  terminalDropNextFrames.clear();
  if (pendingRendererDrops) {
    for (const sessionId of pendingRendererDrops) terminalDropNextFrames.add(sessionId);
  }
  terminalDroppedFrameCounts.clear();
  if (import.meta.env.VITE_ROOST_SMOKE === "1") {
    terminalBlackholeFaults.clear();
    terminalWireDeltaFaults.clear();
    terminalBlackholeDropCounts.clear();
    terminalWireDeltaDropCounts.clear();
    terminalWireDeltaDroppedSeq.clear();
    terminalWireDeltaPostDropSeq.clear();
  }
  terminalGenerationObservation.key = null;
  terminalGenerationObservation.initialized = false;
}
