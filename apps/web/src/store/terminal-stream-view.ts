// Terminal views give UI panes stable leases over one shared session replica.
// They preserve viewport intent and renderer subscriptions while Sync generations change.
// CellTerminal creates these handles, and inbound view-state dispatch enters through this path.
// Command ordering lives beside the view while canonical cell continuity stays in the replica.

import {
  TERMINAL_VIEW_HEARTBEAT_MS,
  clampTerminalGeometry,
} from "@roost/shared/viewport";
import { isPageVisible } from "../lib/pageVisible.ts";
import {
  registerSyncV2GenerationHandler,
  type SyncV2TerminalState,
} from "./sync.ts";
import { clearTerminalChunkTransfer } from "./terminal-stream-chunks.ts";
import {
  clearViewAck,
  changeIntent,
  hasActiveTerminalView,
  publishIntent,
} from "./terminal-stream-view-commands.ts";
import {
  clearTerminalSessionLiveness,
  deliverCanonicalToSubscriber,
  repairStaleTerminalSubscriberOnHeartbeat,
  requestTerminalLivenessChallenge,
  sendLatchedTerminalResync,
  terminalGenerationKey,
  terminalGenerationMatches,
  terminalGenerationToken,
} from "./terminal-stream-replica.ts";
import {
  emitTerminalViewStatus,
  pruneTerminalSessionState,
  terminalBlackholeFaults,
  terminalGenerationObservation,
  terminalSessionReplica,
  terminalSessions,
  terminalWireDeltaFaults,
} from "./terminal-stream-state.ts";
import type {
  BaselineProgress,
  TerminalRendererSubscriber,
  TerminalViewHandle,
  TerminalViewRecord,
} from "./terminal-stream-types.ts";

export { dispatchTerminalViewState } from "./terminal-stream-view-commands.ts";

export function createTerminalView(sessionId: string): TerminalViewHandle {
  const session = terminalSessionReplica(sessionId);
  const viewId = crypto.randomUUID();
  const view: TerminalViewRecord = {
    session,
    viewId,
    revisionFloor: 0n,
    desired: null,
    accepted: null,
    status: null,
    statusListeners: new Set(),
    progressListeners: new Set(),
    progressTimer: null,
    lastProgressKey: null,
    rendererSubscribers: new Set(),
    rollingBack: false,
    viewAckTimer: null,
    heartbeat: null,
    leaseDeadlineMs: null,
    pendingViewAckAtMs: null,
    pendingViewAckGeneration: null,
    pendingViewAckRevision: null,
    disposed: false,
  };
  session.handles.set(viewId, view);
  view.heartbeat = setInterval(() => {
    const desired = view.desired;
    if (view.disposed || !desired?.active) return;
    if (!isPageVisible()) {
      clearViewAck(view);
      clearTerminalSessionLiveness(session, "inactive");
      return;
    }
    const now = Date.now();
    const awaitingReplayBaseline =
      session.requiresFreshBaseline
      && view.status?.status === "pending"
      && view.status.revision === desired.revision
      && view.leaseDeadlineMs !== null
      && view.leaseDeadlineMs > now + TERMINAL_VIEW_HEARTBEAT_MS;
    // A timer deferred while the page was stalled can fire immediately after a
    // redial replay. That replay already holds a live server lease and requests
    // the authoritative baseline; do not race it with another view + resync.
    if (awaitingReplayBaseline) return;
    const accepted = view.status?.status === "accepted"
      && view.status.revision === desired.revision;
    if (publishIntent(view, desired) && accepted) {
      repairStaleTerminalSubscriberOnHeartbeat(session);
    }
  }, TERMINAL_VIEW_HEARTBEAT_MS);

  return {
    sessionId,
    viewId,
    challengeLiveness(): void {
      if (!view.disposed && view.desired?.active) {
        requestTerminalLivenessChallenge(session);
      }
    },
    setViewport(geometry): void {
      const trusted = clampTerminalGeometry(geometry);
      changeIntent(view, true, trusted.cols, trusted.rows);
    },
    setInactive(): void {
      changeIntent(view, false, 0, 0);
    },
    refresh(): void {
      if (!view.disposed && view.desired) publishIntent(view, view.desired);
    },
    subscribeStatus(listener): () => void {
      if (view.disposed) return () => undefined;
      view.statusListeners.add(listener);
      if (view.status) listener(view.status);
      return () => { view.statusListeners.delete(listener); };
    },
    subscribeProgress(listener): () => void {
      if (view.disposed) return () => undefined;
      view.progressListeners.add(listener);
      const current = view.session.assembler.snapshotProgress;
      view.lastProgressKey = baselineProgressKey(current);
      listener(current);
      // One poller per view, alive only while a listener is attached.
      view.progressTimer = setInterval(
        () => emitTerminalViewProgress(view),
        TERMINAL_PROGRESS_POLL_MS,
      );
      return () => {
        view.progressListeners.delete(listener);
        if (view.progressListeners.size === 0 && view.progressTimer !== null) {
          clearInterval(view.progressTimer);
          view.progressTimer = null;
        }
      };
    },
    subscribeRenderer(renderer, onDelivery): () => void {
      if (view.disposed) return () => undefined;
      const subscriber: TerminalRendererSubscriber = {
        sessionId,
        renderer,
        onDelivery: (delivery) => {
          const desired = view.desired;
          const leaseDeadlineMs = view.leaseDeadlineMs;
          if (
            !view.disposed
            && desired?.active
            && leaseDeadlineMs !== null
            && leaseDeadlineMs <= Date.now() + TERMINAL_VIEW_HEARTBEAT_MS
          ) {
            publishIntent(view, desired);
          }
          onDelivery?.(delivery);
        },
        streamId: null,
        gridEpoch: null,
        seq: null,
      };
      session.subscribers.add(subscriber);
      view.rendererSubscribers.add(subscriber);
      if (
        session.baselineReady
        && session.canonical?.streamId === session.expectedStreamId
      ) {
        deliverCanonicalToSubscriber(session, subscriber);
      }
      return () => {
        session.subscribers.delete(subscriber);
        view.rendererSubscribers.delete(subscriber);
      };
    },
    dispose(): void {
      if (view.disposed) return;
      if (view.desired?.active) changeIntent(view, false, 0, 0);
      clearViewAck(view);
      if (!hasActiveTerminalView(session)) {
        clearTerminalSessionLiveness(session, "disposed");
      }
      view.disposed = true;
      if (view.heartbeat !== null) clearInterval(view.heartbeat);
      view.statusListeners.clear();
      view.lastProgressKey = null;
      if (view.progressTimer !== null) clearInterval(view.progressTimer);
      view.progressTimer = null;
      view.progressListeners.clear();
      for (const subscriber of view.rendererSubscribers) {
        session.subscribers.delete(subscriber);
      }
      view.rendererSubscribers.clear();
      session.handles.delete(viewId);
      if (session.handles.size === 0) pruneTerminalSessionState(sessionId);
    },
  };
}

function handleGeneration(state: SyncV2TerminalState | null): void {
  const nextGenerationKey = state ? terminalGenerationKey(state) : null;
  const generationChanged = terminalGenerationObservation.initialized
    && terminalGenerationObservation.key !== nextGenerationKey;
  terminalGenerationObservation.initialized = true;
  terminalGenerationObservation.key = nextGenerationKey;
  if (import.meta.env.VITE_ROOST_SMOKE === "1" && generationChanged) {
    for (const [sessionId, fault] of terminalBlackholeFaults) {
      if (!terminalGenerationMatches(fault.generation, state)) {
        terminalBlackholeFaults.delete(sessionId);
      }
    }
    for (const [sessionId, fault] of terminalWireDeltaFaults) {
      if (!terminalGenerationMatches(fault.generation, state)) {
        terminalWireDeltaFaults.delete(sessionId);
      }
    }
  }
  for (const session of terminalSessions.values()) {
    session.generation = state ? terminalGenerationToken(state) : null;
    clearTerminalChunkTransfer(session);
    if (generationChanged) {
      clearTerminalSessionLiveness(session, "generation_reset");
      if (state !== null) {
        // The replayed view command is this generation's one authoritative
        // baseline request. A resync latch belongs to the prior socket and
        // would otherwise race that replay with a duplicate full frame.
        session.requiresFreshBaseline = true;
        session.baselineReady = false;
        session.resyncLatched = false;
      }
    }
    for (const view of session.handles.values()) {
      if (generationChanged) clearViewAck(view);
      if (view.disposed || !view.desired) continue;
      if (
        generationChanged
        && (
          view.status?.status !== "pending"
          || view.status.revision !== view.desired.revision
        )
      ) {
        emitTerminalViewStatus(view, {
          status: "pending",
          revision: view.desired.revision,
          active: view.desired.active,
        });
      }
      publishIntent(view, view.desired, state);
    }
    if (state?.ready) sendLatchedTerminalResync(session);
  }
}

queueMicrotask(() => registerSyncV2GenerationHandler(handleGeneration));
if (typeof document !== "undefined" && typeof document.addEventListener === "function") {
  document.addEventListener("visibilitychange", () => {
    if (isPageVisible()) return;
    for (const session of terminalSessions.values()) {
      for (const view of session.handles.values()) clearViewAck(view);
      clearTerminalSessionLiveness(session, "inactive");
    }
  });
}

const TERMINAL_PROGRESS_POLL_MS = 200;

function baselineProgressKey(progress: BaselineProgress | null): string {
  return progress === null
    ? "idle"
    : `${progress.snapshotId}:${progress.receivedChunks}/${progress.totalChunks}`;
}

/** Read the replica assembler's current attach progress and fan any change
 * out to the view's subscribers. Chunk pushes land here within one poll;
 * completion and reset surface as a null emission because the assembler's
 * getter goes idle in both cases. */
function emitTerminalViewProgress(view: TerminalViewRecord): void {
  const progress = view.session.assembler.snapshotProgress;
  const key = baselineProgressKey(progress);
  if (key === view.lastProgressKey) return;
  view.lastProgressKey = key;
  for (const listener of view.progressListeners) listener(progress);
}
