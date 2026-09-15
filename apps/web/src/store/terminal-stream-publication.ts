// Where one session's next view/resync command goes, and which socket owns the
// generation stamped on its frames. The local socket wins whenever it holds a
// grant for that session; otherwise a ready Sync socket does. The view, repair
// and retarget owners ask here instead of choosing a transport themselves, so
// exactly one publication path exists per session.

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
import { terminalGenerationToken } from "./terminal-stream-liveness.ts";
import {
  isLocalTerminalGenerationToken,
  localTerminalGenerationToken,
  localTerminalTransport,
} from "./terminal-stream-transport.ts";
import type { TerminalGenerationToken } from "./terminal-stream-types.ts";

export interface TerminalPublicationTarget {
  readonly token: TerminalGenerationToken;
  readonly domainGeneration: bigint;
  readonly local: boolean;
  publishView(command: TerminalViewCommand): boolean;
  publishResync(command: TerminalResyncCommand): boolean;
}

export function currentTerminalGenerationToken(
  sessionId: string,
  sync: SyncV2TerminalState | null = currentSyncV2TerminalState(),
): TerminalGenerationToken | null {
  return localTerminalGenerationToken(sessionId)
    ?? (sync ? terminalGenerationToken(sync) : null);
}

/** Where this session's next command goes, or null when neither transport can
 * carry it. */
export function terminalPublicationTarget(
  sessionId: string,
  sync: SyncV2TerminalState | null = currentSyncV2TerminalState(),
): TerminalPublicationTarget | null {
  if (localTerminalGenerationToken(sessionId)) return localTarget();
  return syncTarget(sync);
}

/** The target that still owns an already-stamped generation — used to release a
 * view key on the transport it is leaving. */
export function terminalTransportTargetForToken(
  token: TerminalGenerationToken,
  sync: SyncV2TerminalState | null = currentSyncV2TerminalState(),
): TerminalPublicationTarget | null {
  return isLocalTerminalGenerationToken(token) ? localTarget() : syncTarget(sync);
}

/** Replace the socket that owns this generation. A local token can only be
 * recovered by the local socket; a Sync redial would not touch it. */
export function requestTerminalGenerationRecovery(
  owner: TerminalGenerationToken,
  reason: TerminalGenerationRecoveryReason,
): boolean {
  if (isLocalTerminalGenerationToken(owner)) {
    return localTerminalTransport()?.redial(reason) ?? false;
  }
  return requestSyncGenerationRecovery(owner, reason);
}

function localTarget(): TerminalPublicationTarget | null {
  const owner = localTerminalTransport();
  const token = owner?.generationToken() ?? null;
  if (!owner || !token) return null;
  return {
    token,
    // The local socket is its own fence; the worker never reads this field.
    domainGeneration: 0n,
    local: true,
    publishView: (command) => owner.publishView(command),
    publishResync: (command) => owner.publishResync(command),
  };
}

function syncTarget(
  sync: SyncV2TerminalState | null,
): TerminalPublicationTarget | null {
  if (!sync?.ready) return null;
  return {
    token: terminalGenerationToken(sync),
    domainGeneration: sync.domainGeneration,
    local: false,
    publishView: (command) => sendSyncV2Command({ case: "terminalView", value: command }),
    publishResync: (command) => sendSyncV2Command({ case: "terminalResync", value: command }),
  };
}
