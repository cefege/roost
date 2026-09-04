// Sync snapshots are valid only for the socket and domain generation that requested them.
// This module owns that token check, the domain-ready command, and subscribed waiters.
// Hydration and inbound leaves collaborate through these operations rather than bypassing them.
// Keeping publication here makes stale reconnect results harmless before they touch Solid state.

import { create, toBinary } from "@bufbuild/protobuf";
import {
  SyncClientFrameSchema,
  SyncDomainReadyCommandSchema,
  SyncDomain,
  type FirehoseFrame,
  type SyncClientFrame,
} from "@roost/shared/proto/sync_pb";
import { markPhase } from "../lib/diag.ts";
import {
  _currentLiveSyncLink,
  _notifySyncV2Generation,
  currentSyncV2TerminalState,
  isCurrentSyncDomainToken,
  type SyncDomainToken,
  type SyncV2TerminalState,
} from "./sync-link-state.ts";

export type SyncV2Control = Extract<
  FirehoseFrame["frame"],
  {
    case:
      | "inputAccepted"
      | "inputRejected"
      | "inputAmbiguous";
  }
>;

export interface SyncSubscribedState {
  readonly socketGeneration: number;
  readonly socketId: string;
  readonly processEpoch: string;
}

export interface SyncDomainSnapshot {
  readonly apply: () => void;
  readonly snapshotToken?: string;
}

export type SyncDomainHydrator = (
  token: SyncDomainToken,
) => Promise<SyncDomainSnapshot | null>;

const v2ControlHandlers = new Set<(
  control: SyncV2Control,
  state: SyncV2TerminalState,
) => void>();
const subscribedWaiters = new Set<(state: SyncSubscribedState) => void>();

export function registerSyncV2ControlHandler(
  handler: (control: SyncV2Control, state: SyncV2TerminalState) => void,
): () => void {
  v2ControlHandlers.add(handler);
  return () => { v2ControlHandlers.delete(handler); };
}

export function sendSyncV2Command(command: SyncClientFrame["command"]): boolean {
  const link = _currentLiveSyncLink();
  if (
    !link
    || !link.v2
    || !link.accepting
    || link.ws.readyState !== WebSocket.OPEN
    || command.case === undefined
  ) return false;
  try {
    link.ws.send(toBinary(SyncClientFrameSchema, create(SyncClientFrameSchema, {
      socketId: link.v2.socketId,
      command,
    })));
    return true;
  } catch {
    return false;
  }
}

export function applySyncDomainSnapshot(
  token: SyncDomainToken,
  snapshot: SyncDomainSnapshot,
): boolean {
  if (!isCurrentSyncDomainToken(token)) return false;
  const link = _currentLiveSyncLink()!;
  const domain = link.v2!.domains.get(token.domain)!;
  if (!domain.subscribed) return false;
  snapshot.apply();
  markPhase("snapshot_applied", {
    domain: SyncDomain[token.domain],
    generation: token.domainGeneration,
  });
  if (!sendSyncV2Command({
    case: "domainReady",
    value: create(SyncDomainReadyCommandSchema, {
      domain: token.domain,
      generation: token.domainGeneration,
      snapshotToken: snapshot.snapshotToken,
    }),
  })) return false;
  if (!isCurrentSyncDomainToken(token)) return false;
  domain.ready = true;
  if (token.domain === SyncDomain.TERMINAL) {
    _notifySyncV2Generation(currentSyncV2TerminalState());
  }
  return true;
}

export function waitForSyncSubscribed(timeoutMs: number): Promise<SyncSubscribedState | null> {
  const link = _currentLiveSyncLink();
  if (link?.v2) {
    return Promise.resolve({
      socketGeneration: link.gen,
      socketId: link.v2.socketId,
      processEpoch: link.v2.processEpoch,
    });
  }
  const { promise, resolve } = Promise.withResolvers<SyncSubscribedState | null>();
  let settled = false;
  let timer: Timer;
  const accept = (state: SyncSubscribedState): void => {
    if (settled) return;
    settled = true;
    clearTimeout(timer);
    subscribedWaiters.delete(accept);
    resolve(state);
  };
  subscribedWaiters.add(accept);
  timer = setTimeout(() => {
    if (settled) return;
    settled = true;
    subscribedWaiters.delete(accept);
    resolve(null);
  }, timeoutMs);
  return promise;
}

export function _resolveSyncSubscribedWaiters(state: SyncSubscribedState): void {
  for (const resolve of subscribedWaiters) resolve(state);
  subscribedWaiters.clear();
}

export function _dispatchSyncV2Control(
  control: SyncV2Control,
  state: SyncV2TerminalState,
): void {
  for (const handler of v2ControlHandlers) handler(control, state);
}
