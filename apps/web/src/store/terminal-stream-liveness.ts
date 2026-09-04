// Terminal liveness proves that a visible renderer still receives its canonical stream.
// Heartbeats and idle probes call this module to request an ordered coordinator rebaseline.
// Generation tokens keep every repair timer and retry scoped to its owning Sync socket.
// The session replica supplies continuity state while the view registry supplies active leases.

import { create } from "@bufbuild/protobuf";
import { signal } from "@roost/shared/diag";
import { TerminalResyncCommandSchema } from "@roost/shared/proto/sync_pb";
import {
  TERMINAL_FOREGROUND_IDLE_PROBE_MS,
  TERMINAL_FOREGROUND_PROBE_DEADLINE_MS,
  TERMINAL_VIEW_HEARTBEAT_MS,
} from "@roost/shared/viewport";
import { isPageVisible } from "../lib/pageVisible.ts";
import {
  currentSyncV2TerminalState,
  requestSyncGenerationRecovery,
  sendSyncV2Command,
  type SyncV2TerminalState,
} from "./sync.ts";
import { clearTerminalChunkTransfer } from "./terminal-stream-chunks.ts";
import type {
  TerminalGenerationToken,
  TerminalOutboundCommand,
  TerminalSessionReplica,
  TerminalViewRecord,
} from "./terminal-stream-types.ts";

export function terminalGenerationToken(
  state: SyncV2TerminalState,
): TerminalGenerationToken {
  return {
    socketGeneration: state.socketGeneration,
    socketId: state.socketId,
    processEpoch: state.processEpoch,
    domainGeneration: state.domainGeneration,
  };
}

export function terminalGenerationMatches(
  token: TerminalGenerationToken | null,
  state: SyncV2TerminalState | TerminalGenerationToken | null,
): boolean {
  return token !== null
    && state !== null
    && token.socketGeneration === state.socketGeneration
    && token.socketId === state.socketId
    && token.processEpoch === state.processEpoch
    && token.domainGeneration === state.domainGeneration;
}

export function terminalGenerationKey(state: SyncV2TerminalState): string {
  return [
    state.socketGeneration,
    state.socketId,
    state.processEpoch,
    state.domainGeneration,
  ].join("\u0000");
}

export function activeTerminalResyncView(
  session: TerminalSessionReplica,
): TerminalViewRecord | null {
  for (const view of session.handles.values()) {
    if (!view.disposed && view.desired?.active) return view;
  }
  return null;
}

export function clearTerminalRepairLatch(session: TerminalSessionReplica): void {
  session.resyncLatched = false;
  session.resyncSentGeneration = null;
  session.resyncRetryGeneration = null;
  session.resyncRetryAtMs = null;
  session.resyncLatchedAtMs = null;
  session.resyncLatchGeneration = null;
}

export function clearTerminalSessionLiveness(
  session: TerminalSessionReplica,
  outcome: TerminalSessionReplica["repairOutcome"],
): void {
  clearTimeout(session.idleProbeTimer ?? undefined);
  session.idleProbeTimer = null;
  clearTimeout(session.proofDeadlineTimer ?? undefined);
  session.proofDeadlineTimer = null;
  session.lastAcceptedFrameAtMs = null;
  session.lastAcceptedFrameGeneration = null;
  session.proofChallengeAtMs = null;
  session.proofChallengeGeneration = null;
  session.repairAttempts = 0;
  session.repairOutcome = outcome;
  clearTerminalRepairLatch(session);
}

export function requestTerminalLivenessChallenge(
  session: TerminalSessionReplica,
): boolean {
  const sync = currentSyncV2TerminalState();
  const owner = session.generation;
  const view = activeForegroundTerminalView(session);
  if (
    !sync?.ready
    || !owner
    || !view
    || !session.baselineReady
    || !session.expectedStreamId
    || !terminalGenerationMatches(owner, sync)
    || (
      session.proofChallengeAtMs !== null
      && terminalGenerationMatches(session.proofChallengeGeneration, owner)
    )
  ) return false;
  if (!sendTerminalResyncCommand(session, view, sync)) return false;
  session.repairAttempts++;
  session.repairOutcome = "requested";
  signal("cell.foreground_stall", {
    sid: session.sessionId,
    stream_id: session.expectedStreamId,
    layer: "terminal_proof",
    action: "resync",
    cooldownKey: session.sessionId,
  });
  armTerminalProofDeadline(session, owner, performance.now());
  return true;
}

export function armTerminalForegroundIdleProbe(
  session: TerminalSessionReplica,
): void {
  if (session.idleProbeTimer !== null) return;
  const owner = session.generation;
  if (
    !owner
    || !session.baselineReady
    || !activeForegroundTerminalView(session)
  ) return;
  const startedAt = terminalGenerationMatches(session.lastAcceptedFrameGeneration, owner)
    ? (session.lastAcceptedFrameAtMs ?? performance.now())
    : performance.now();
  const dueAt = startedAt + TERMINAL_FOREGROUND_IDLE_PROBE_MS;
  const timer = setTimeout(() => {
    if (session.idleProbeTimer !== timer) return;
    session.idleProbeTimer = null;
    const sync = currentSyncV2TerminalState();
    if (
      !activeForegroundTerminalView(session)
      || !terminalGenerationMatches(session.generation, owner)
      || !terminalGenerationMatches(owner, sync)
    ) {
      clearTerminalSessionLiveness(session, "inactive");
      return;
    }
    if (
      terminalGenerationMatches(session.lastAcceptedFrameGeneration, owner)
      && session.lastAcceptedFrameAtMs !== null
      && session.lastAcceptedFrameAtMs > startedAt
    ) {
      armTerminalForegroundIdleProbe(session);
      return;
    }
    requestTerminalLivenessChallenge(session);
  }, Math.max(0, dueAt - performance.now()));
  session.idleProbeTimer = timer;
}

export function sendLatchedTerminalResync(
  session: TerminalSessionReplica,
  mode: "initial" | "heartbeat-retry" = "initial",
): void {
  if (!session.resyncLatched || !session.expectedStreamId) return;
  const sync = currentSyncV2TerminalState();
  const view = activeForegroundTerminalView(session);
  const owner = session.resyncLatchGeneration;
  if (
    !sync?.ready
    || !view
    || !owner
    || !terminalGenerationMatches(session.generation, sync)
    || !terminalGenerationMatches(owner, sync)
  ) return;
  const key = terminalGenerationKey(sync);
  if (session.resyncSentGeneration === key) {
    if (mode !== "heartbeat-retry") return;
    const now = Date.now();
    if (
      session.resyncRetryGeneration === key
      && session.resyncRetryAtMs !== null
      && now - session.resyncRetryAtMs < TERMINAL_VIEW_HEARTBEAT_MS
    ) return;
  }
  if (sendTerminalResyncCommand(session, view, sync)) {
    session.resyncSentGeneration = key;
    session.resyncRetryGeneration = key;
    session.resyncRetryAtMs = Date.now();
    session.repairAttempts++;
    session.repairOutcome = "requested";
    if (session.resyncLatchedAtMs !== null) {
      armTerminalProofDeadline(session, owner, session.resyncLatchedAtMs);
    }
  }
}

export function requestTerminalResync(
  session: TerminalSessionReplica,
  reason: string,
  mode: "initial" | "heartbeat-retry" = "initial",
  owner = session.generation,
  reportGap = true,
): void {
  if (!owner || !terminalGenerationMatches(session.generation, owner)) return;
  clearTerminalChunkTransfer(session);
  if (!session.resyncLatched) {
    session.resyncLatchedAtMs = performance.now();
    session.resyncLatchGeneration = owner;
    session.resyncLatched = true;
    if (reportGap) {
      signal("cell.seq_gap", {
        sid: session.sessionId,
        stream_id: session.expectedStreamId,
        reason: reason.slice(0, 200),
        cooldownKey: session.sessionId,
      });
    }
  }
  sendLatchedTerminalResync(session, mode);
}

/** An exact view heartbeat is also the renderer's applied-sequence proof.
 * The session replica may be ahead when a renderer rejected or a smoke probe
 * deliberately suppressed one delivery. Repair through the ordinary
 * coordinator rebaseline path rather than copying around that path locally. */
export function repairStaleTerminalSubscriberOnHeartbeat(
  session: TerminalSessionReplica,
): void {
  if (session.assembler.activeSnapshotId !== null) return;
  const canonical = session.canonical;
  if (!session.baselineReady || !canonical) {
    requestTerminalResync(
      session,
      "terminal baseline was still missing at heartbeat",
      "heartbeat-retry",
      session.generation,
      false,
    );
    return;
  }
  if (session.resyncLatched) {
    requestTerminalResync(
      session,
      "terminal rebaseline remained unanswered at heartbeat",
      "heartbeat-retry",
    );
    return;
  }
  for (const subscriber of session.subscribers) {
    if (
      subscriber.streamId !== canonical.streamId
      || subscriber.gridEpoch !== canonical.gridEpoch
      || subscriber.seq !== canonical.seq
    ) {
      requestTerminalResync(
        session,
        "terminal renderer applied sequence trailed the canonical replica at heartbeat",
        "heartbeat-retry",
        session.generation,
        false,
      );
      return;
    }
  }
}

function activeForegroundTerminalView(
  session: TerminalSessionReplica,
): TerminalViewRecord | null {
  return isPageVisible() ? activeTerminalResyncView(session) : null;
}

function sendTerminalResyncCommand(
  session: TerminalSessionReplica,
  view: TerminalViewRecord,
  sync: SyncV2TerminalState,
): boolean {
  if (!session.expectedStreamId) return false;
  const canonical = session.canonical;
  const outbound: TerminalOutboundCommand = {
    case: "terminalResync",
    value: create(TerminalResyncCommandSchema, {
      viewId: view.viewId,
      sessionId: session.sessionId,
      streamId: session.expectedStreamId,
      gridEpoch: canonical?.gridEpoch ?? "",
      seq: BigInt(canonical?.seq ?? 0),
      domainGeneration: sync.domainGeneration,
    }),
  };
  return sendSyncV2Command(outbound);
}

function armTerminalProofDeadline(
  session: TerminalSessionReplica,
  owner: TerminalGenerationToken,
  challengedAt = performance.now(),
): void {
  if (
    session.proofDeadlineTimer !== null
    && terminalGenerationMatches(session.proofChallengeGeneration, owner)
  ) return;
  clearTimeout(session.proofDeadlineTimer ?? undefined);
  session.proofChallengeAtMs = challengedAt;
  session.proofChallengeGeneration = owner;
  const timer = setTimeout(() => {
    if (session.proofDeadlineTimer !== timer) return;
    session.proofDeadlineTimer = null;
    const sync = currentSyncV2TerminalState();
    if (
      !activeForegroundTerminalView(session)
      || !terminalGenerationMatches(session.generation, owner)
      || !terminalGenerationMatches(session.proofChallengeGeneration, owner)
      || !terminalGenerationMatches(owner, sync)
    ) {
      clearTerminalSessionLiveness(session, "inactive");
      return;
    }
    session.repairOutcome = "escalated";
    signal("cell.foreground_stall", {
      sid: session.sessionId,
      stream_id: session.expectedStreamId,
      layer: "terminal_proof",
      action: "redial",
      age_ms: Math.max(0, performance.now() - challengedAt),
      cooldownKey: session.sessionId,
    });
    requestSyncGenerationRecovery(owner, "terminal-proof-timeout");
  }, Math.max(
    0,
    challengedAt + TERMINAL_FOREGROUND_PROBE_DEADLINE_MS - performance.now(),
  ));
  session.proofDeadlineTimer = timer;
}
