import { CellGridChunkAssembler } from "@roost/shared/cell";
import type {
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

export function pruneTerminalSessionState(sessionId: string): void {
  const session = terminalSessions.get(sessionId);
  if (session) {
    session.assembler.reset();
    clearTimeout(session.chunkTimer ?? undefined);
    session.chunkTimer = null;
    for (const view of session.handles.values()) {
      if (view.heartbeat !== null) clearInterval(view.heartbeat);
    }
    session.handles.clear();
    session.subscribers.clear();
    terminalSessions.delete(sessionId);
  }
  terminalFrameCounts.delete(sessionId);
  terminalFullFrameCounts.delete(sessionId);
  terminalFullFrameScrollbackRows.delete(sessionId);
  terminalGridEpochs.delete(sessionId);
  terminalDropNextFrames.delete(sessionId);
  terminalDroppedFrameCounts.delete(sessionId);
}

export function resetTerminalStreamState(): void {
  for (const sessionId of Array.from(terminalSessions.keys())) {
    pruneTerminalSessionState(sessionId);
  }
  terminalFrameCounts.clear();
  terminalFullFrameCounts.clear();
  terminalFullFrameScrollbackRows.clear();
  terminalGridEpochs.clear();
  clearPersistedTerminalRendererDrops();
  terminalDropNextFrames.clear();
  terminalDroppedFrameCounts.clear();
  terminalGenerationObservation.key = null;
  terminalGenerationObservation.initialized = false;
}
