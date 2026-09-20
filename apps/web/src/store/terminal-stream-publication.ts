// Chooses the one route that may publish a session's views and resyncs. Direct
// routes are elected by terminal-stream-transport; Sync remains the ready
// fallback. Consumers never infer a route from a sentinel process epoch.

import type {
  TerminalResyncCommand,
  TerminalViewCommand,
} from "@roost/shared/proto/sync_pb";
import {
  currentSyncV2TerminalState,
  requestSyncGenerationRecovery,
  sendSyncV2Command,
  type SyncV2TerminalState,
  type TerminalGenerationRecoveryReason,
} from "./sync.ts";
import {
  terminalGenerationMatches,
  terminalGenerationToken,
} from "./terminal-stream-liveness.ts";
import {
  terminalDirectRegistry,
  type TerminalDirectConnection,
} from "./terminal-stream-transport.ts";
import type {
  TerminalGenerationToken,
  TerminalTransportKind,
} from "./terminal-stream-types.ts";

export interface TerminalPublicationTarget {
  readonly token: TerminalGenerationToken;
  readonly domainGeneration: bigint;
  readonly transportKind: TerminalTransportKind;
  readonly workerFp: string | null;
  publishView(command: TerminalViewCommand): boolean;
  publishResync(command: TerminalResyncCommand): boolean;
}

export function currentTerminalGenerationToken(
  sessionId: string,
  sync: SyncV2TerminalState | null = currentSyncV2TerminalState(),
): TerminalGenerationToken | null {
  const direct = terminalDirectRegistry.activeForSession(sessionId);
  return direct?.token() ?? (sync ? terminalGenerationToken(sync) : null);
}

/** Where this session's next command goes, or null when neither route can
 * carry it. */
export function terminalPublicationTarget(
  sessionId: string,
  sync: SyncV2TerminalState | null = currentSyncV2TerminalState(),
): TerminalPublicationTarget | null {
  const direct = terminalDirectRegistry.activeForSession(sessionId);
  return directTarget(direct) ?? syncTarget(sync);
}

/** The target that still owns an already-stamped generation — used to release a
 * view key on the route it is leaving. */
export function terminalTransportTargetForToken(
  token: TerminalGenerationToken,
  sync: SyncV2TerminalState | null = currentSyncV2TerminalState(),
): TerminalPublicationTarget | null {
  if (token.transportKind !== "sync") {
    return directTarget(terminalDirectRegistry.targetForToken(token));
  }
  const target = syncTarget(sync);
  return target && terminalGenerationMatches(target.token, token) ? target : null;
}

/** Replaces only the exact route that owns a stalled generation. */
export function requestTerminalGenerationRecovery(
  owner: TerminalGenerationToken,
  reason: TerminalGenerationRecoveryReason,
): boolean {
  if (owner.transportKind !== "sync") {
    const direct = terminalDirectRegistry.targetForToken(owner);
    if (!direct) return false;
    direct.close(reason);
    return true;
  }
  return requestSyncGenerationRecovery(owner, reason);
}

function directTarget(
  connection: TerminalDirectConnection | null,
): TerminalPublicationTarget | null {
  const token = connection?.token() ?? null;
  if (
    !connection
    || !token
    || terminalDirectRegistry.targetForToken(token) !== connection
  ) return null;
  return {
    token,
    domainGeneration: token.domainGeneration,
    transportKind: token.transportKind,
    workerFp: token.workerFp,
    publishView: (command) => connection.publishView(command),
    publishResync: (command) => connection.publishResync(command),
  };
}

function syncTarget(
  sync: SyncV2TerminalState | null,
): TerminalPublicationTarget | null {
  if (!sync?.ready) return null;
  return {
    token: terminalGenerationToken(sync),
    domainGeneration: sync.domainGeneration,
    transportKind: "sync",
    workerFp: null,
    publishView: (command) => sendSyncV2Command({ case: "terminalView", value: command }),
    publishResync: (command) => sendSyncV2Command({ case: "terminalResync", value: command }),
  };
}
