// Sessions follow the elected direct route or ready Sync fallback. This module
// owns ordinary generation retargeting and view-id rotation; staged direct
// promotion has already atomically installed its baseline before notification.

import { create } from "@bufbuild/protobuf";
import { TerminalViewCommandSchema } from "@roost/protocol/proto/sync_pb";
import { isPageVisible } from "../lib/pageVisible.ts";
import {
  currentSyncV2TerminalState,
  registerSyncV2GenerationHandler,
  type SyncV2TerminalState,
} from "./sync.ts";
import { clearTerminalChunkTransfer } from "./terminal-stream-chunks.ts";
import {
  clearTerminalSessionLiveness,
  sendLatchedTerminalResync,
  terminalGenerationKey,
  terminalGenerationMatches,
  terminalGenerationToken,
} from "./terminal-stream-replica.ts";
import {
  beginTerminalViewRenewalBatch,
  cancelTerminalViewRenewal,
  endTerminalViewRenewalBatch,
  invalidateTerminalViewRenewals,
} from "./terminal-stream-renewal-scheduler.ts";
import {
  emitTerminalViewStatus,
  notifyTerminalTransportStateChange,
  terminalBlackholeFaults,
  terminalGenerationObservation,
  terminalSessions,
  terminalWireDeltaFaults,
} from "./terminal-stream-state.ts";
import { terminalTransportTargetForToken } from "./terminal-stream-publication.ts";
import {
  terminalDirectRegistry,
  type TerminalDirectRegistryEvent,
} from "./terminal-stream-transport.ts";
import type {
  TerminalGenerationToken,
  TerminalSessionReplica,
  TerminalViewRecord,
} from "./terminal-stream-types.ts";
import { clearViewAck, publishIntent } from "./terminal-stream-view-commands.ts";

export function installTerminalTransportRetarget(): void {
  registerSyncV2GenerationHandler(handleSyncGeneration);
  terminalDirectRegistry.subscribe(handleDirectRegistryEvent);
}

function handleSyncGeneration(state: SyncV2TerminalState | null): void {
  const nextGenerationKey = state ? terminalGenerationKey(state) : null;
  const generationChanged = terminalGenerationObservation.initialized
    && terminalGenerationObservation.key !== nextGenerationKey;
  terminalGenerationObservation.initialized = true;
  terminalGenerationObservation.key = nextGenerationKey;
  if (generationChanged) invalidateTerminalViewRenewals();
  if (import.meta.env.VITE_ROOST_SMOKE === "1" && generationChanged) {
    for (const [sessionId, fault] of terminalBlackholeFaults) {
      if (
        fault.generation.transportKind === "sync"
        && !terminalGenerationMatches(fault.generation, state)
      ) terminalBlackholeFaults.delete(sessionId);
    }
    for (const [sessionId, fault] of terminalWireDeltaFaults) {
      if (
        fault.generation.transportKind === "sync"
        && !terminalGenerationMatches(fault.generation, state)
      ) terminalWireDeltaFaults.delete(sessionId);
    }
  }
  // A ready flip carries the same generation key and still owes every Sync
  // view its authoritative replay. Elected direct routes remain undisturbed.
  retargetTerminalSessions(state, generationChanged, true);
}

function handleDirectRegistryEvent(event: TerminalDirectRegistryEvent): void {
  if (event.kind !== "route_lost") return;
  const session = terminalSessions.get(event.sessionId);
  if (
    !session
    || !terminalGenerationMatches(session.generation, event.token)
  ) return;
  retargetSession(session, currentSyncV2TerminalState(), false, false);
}

function retargetTerminalSessions(
  state: SyncV2TerminalState | null,
  syncGenerationChanged: boolean,
  replayUnchanged: boolean,
): void {
  beginTerminalViewRenewalBatch();
  try {
    for (const session of terminalSessions.values()) {
      retargetSession(session, state, syncGenerationChanged, replayUnchanged);
    }
  } finally {
    endTerminalViewRenewalBatch();
  }
}

function retargetSession(
  session: TerminalSessionReplica,
  state: SyncV2TerminalState | null,
  syncGenerationChanged: boolean,
  replayUnchanged: boolean,
): void {
  const previous = session.generation;
  const direct = terminalDirectRegistry.activeForSession(session.sessionId);
  const directToken = direct?.token() ?? null;
  const token = directToken ?? (state ? terminalGenerationToken(state) : null);
  const wasDirect = previous?.transportKind === "loopback"
    || previous?.transportKind === "webrtc";
  const changed = directToken !== null || wasDirect
    ? !terminalGenerationMatches(previous, token)
    : syncGenerationChanged;
  if (!changed && (directToken !== null || !replayUnchanged)) return;
  const transportChanged = previous !== null
    && (
      previous.transportKind !== "sync"
        ? token === null
          || previous.transportKind !== token.transportKind
          || previous.workerFp !== token.workerFp
        : token !== null && token.transportKind !== "sync"
    );
  session.generation = token;
  clearTerminalChunkTransfer(session);
  if (changed) {
    clearTerminalSessionLiveness(session, "generation_reset");
    if (token !== null) {
      // The replayed view command is this generation's one authoritative
      // baseline request. A resync latch belongs to the prior route and would
      // otherwise race that replay with a duplicate full frame.
      session.requiresFreshBaseline = true;
      session.baselineReady = false;
      session.resyncLatched = false;
    }
  }
  // Snapshot: rotating a view re-keys session.handles, and a Map visits keys
  // added while it is being iterated.
  for (const view of [...session.handles.values()]) {
    if (changed) clearViewAck(view);
    if (view.disposed || !view.desired) continue;
    if (transportChanged && previous) rotateViewTransport(view, previous, state);
    if (view.desired.active && !isPageVisible()) {
      cancelTerminalViewRenewal(view);
      continue;
    }
    if (
      changed
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
  if (changed) notifyTerminalTransportStateChange();
  if (directToken !== null || state?.ready) sendLatchedTerminalResync(session);
}

/** Release this view's key on the route it is leaving, then take a fresh
 * identity for a different carrier. Two live carriers sharing one viewer key
 * would otherwise contend for the same worker view record. */
function rotateViewTransport(
  view: TerminalViewRecord,
  previous: TerminalGenerationToken,
  state: SyncV2TerminalState | null,
): void {
  const leaving = terminalTransportTargetForToken(previous, state);
  if (leaving) {
    leaving.publishView(create(TerminalViewCommandSchema, {
      viewId: view.viewId,
      sessionId: view.session.sessionId,
      cols: 0,
      rows: 0,
      revision: ++view.revisionFloor,
      active: false,
      domainGeneration: leaving.domainGeneration,
    }));
  }
  const oldViewId = view.viewId;
  terminalDirectRegistry.setViewDemand(
    view.session.workerFp,
    view.session.sessionId,
    oldViewId,
    false,
  );
  view.session.handles.delete(oldViewId);
  view.viewId = crypto.randomUUID();
  view.session.handles.set(view.viewId, view);
  if (view.desired?.active) {
    terminalDirectRegistry.setViewDemand(
      view.session.workerFp,
      view.session.sessionId,
      view.viewId,
      true,
    );
  }
}
