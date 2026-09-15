// Sessions follow their transport. This module retargets every session replica
// when the Sync generation moves or the local socket's grant set changes, and
// it is the only place a view's published identity rotates. A transport change
// IS a generation change: liveness resets, the view relinquishes its old key on
// the transport it is leaving and republishes under a fresh view id on the new
// one, so the owner mints a new stream whose baseline precedes any delta.

import { create } from "@bufbuild/protobuf";
import { TerminalViewCommandSchema } from "@roost/shared/proto/sync_pb";
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
  terminalBlackholeFaults,
  terminalGenerationObservation,
  terminalSessions,
  terminalWireDeltaFaults,
} from "./terminal-stream-state.ts";
import { terminalTransportTargetForToken } from "./terminal-stream-publication.ts";
import {
  isLocalTerminalGenerationToken,
  localTerminalGenerationToken,
  registerTerminalLocalTransportHandler,
} from "./terminal-stream-transport.ts";
import type {
  TerminalGenerationToken,
  TerminalSessionReplica,
  TerminalViewRecord,
} from "./terminal-stream-types.ts";
import { clearViewAck, publishIntent } from "./terminal-stream-view-commands.ts";

export function installTerminalTransportRetarget(): void {
  registerSyncV2GenerationHandler(handleSyncGeneration);
  registerTerminalLocalTransportHandler(handleLocalTransportChange);
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
  // A ready flip carries the same generation key and still owes every view its
  // authoritative replay, so an unchanged Sync generation republishes.
  retargetTerminalSessions(state, generationChanged, true);
}

/** The local socket's grant set or generation moved: only the sessions whose
 * owning transport actually changed are disturbed. */
function handleLocalTransportChange(): void {
  retargetTerminalSessions(currentSyncV2TerminalState(), false, false);
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
  const localToken = localTerminalGenerationToken(session.sessionId);
  const token = localToken ?? (state ? terminalGenerationToken(state) : null);
  const wasLocal = isLocalTerminalGenerationToken(previous);
  const changed = localToken !== null || wasLocal
    ? !terminalGenerationMatches(previous, token)
    : syncGenerationChanged;
  if (!changed && !replayUnchanged) return;
  const transportChanged = previous !== null && wasLocal !== (localToken !== null);
  session.generation = token;
  clearTerminalChunkTransfer(session);
  if (changed) {
    clearTerminalSessionLiveness(session, "generation_reset");
    if (token !== null) {
      // The replayed view command is this generation's one authoritative
      // baseline request. A resync latch belongs to the prior socket and
      // would otherwise race that replay with a duplicate full frame.
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
  if (localToken !== null || state?.ready) sendLatchedTerminalResync(session);
}

/** Release this view's key on the transport it is leaving, then take a fresh
 * identity for the new one. Two live sockets sharing one viewer key would
 * otherwise contend for the same registry record and the newcomer would be
 * refused as "owned by another live socket". */
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
  view.session.handles.delete(view.viewId);
  view.viewId = crypto.randomUUID();
  view.session.handles.set(view.viewId, view);
}
