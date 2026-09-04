// A Sync callback must prove it still belongs to the current socket generation.
// This module owns that identity record and retires every timer with its link.
// Domain and redial leaves use these operations instead of keeping rival globals.
// Terminal generation listeners are notified whenever the live link changes state.

import { SyncDomain } from "@roost/shared/proto/sync_pb";
import type {
  StaleWatchdog,
  SyncLinkLiveness,
} from "./sync-watchdog.ts";

export type SyncAbortReason =
  | "visibility"
  | "manual"
  | "stale"
  | "flow"
  | "terminal-liveness"
  | null;

export interface SyncV2DomainState {
  generation: bigint;
  subscribed: boolean;
  ready: boolean;
}

interface SyncV2LinkState {
  socketId: string;
  processEpoch: string;
  domains: Map<SyncDomain, SyncV2DomainState>;
  routableChunks: Map<string, {
    count: number;
    chunks: Array<string[] | undefined>;
  }>;
}

export interface LiveSyncLink {
  ws: WebSocket;
  gen: number;
  abortReason: SyncAbortReason;
  accepting: boolean;
  resolveClosed: () => void;
  expectsV2: boolean;
  openTimer: Timer | null;
  closeEscapeTimer: Timer | null;
  watchdog: StaleWatchdog | null;
  v2: SyncV2LinkState | null;
}

export interface SyncDomainToken {
  readonly socketGeneration: number;
  readonly socketId: string;
  readonly processEpoch: string;
  readonly domain: SyncDomain;
  readonly domainGeneration: bigint;
}

export interface SyncV2TerminalState {
  readonly socketGeneration: number;
  readonly socketId: string;
  readonly processEpoch: string;
  readonly domainGeneration: bigint;
  readonly ready: boolean;
}

const WS_CLOSE_ESCAPE_MS = 5_000;
let liveSyncLink: LiveSyncLink | null = null;
let syncSocketGeneration = 0;
const v2GenerationHandlers = new Set<(
  state: SyncV2TerminalState | null,
) => void>();

export function _currentLiveSyncLink(): LiveSyncLink | null {
  return liveSyncLink;
}

export function _installLiveSyncLink(link: LiveSyncLink): void {
  liveSyncLink = link;
}

export function _clearLiveSyncLink(link: LiveSyncLink): void {
  if (liveSyncLink === link) liveSyncLink = null;
}

export function _allocateSyncSocketGeneration(): number {
  syncSocketGeneration += 1;
  return syncSocketGeneration;
}

/** How many Sync sockets this tab has dialed. */
export function syncWsGeneration(): number {
  return syncSocketGeneration;
}

export function currentSyncDomainToken(domain: SyncDomain): SyncDomainToken | null {
  const link = liveSyncLink;
  const domainState = link?.v2?.domains.get(domain);
  if (
    !link
    || !link.accepting
    || link.ws.readyState !== WebSocket.OPEN
    || !link.v2
    || !domainState
  ) return null;
  return {
    socketGeneration: link.gen,
    socketId: link.v2.socketId,
    processEpoch: link.v2.processEpoch,
    domain,
    domainGeneration: domainState.generation,
  };
}

export function currentSyncV2TerminalState(): SyncV2TerminalState | null {
  const token = currentSyncDomainToken(SyncDomain.TERMINAL);
  if (!token) return null;
  const ready = liveSyncLink?.v2?.domains.get(SyncDomain.TERMINAL)?.ready ?? false;
  return {
    socketGeneration: token.socketGeneration,
    socketId: token.socketId,
    processEpoch: token.processEpoch,
    domainGeneration: token.domainGeneration,
    ready,
  };
}

export function isCurrentSyncDomainToken(token: SyncDomainToken): boolean {
  const current = currentSyncDomainToken(token.domain);
  return !!current
    && current.socketGeneration === token.socketGeneration
    && current.socketId === token.socketId
    && current.processEpoch === token.processEpoch
    && current.domainGeneration === token.domainGeneration;
}

export function registerSyncV2GenerationHandler(
  handler: (state: SyncV2TerminalState | null) => void,
): () => void {
  v2GenerationHandlers.add(handler);
  handler(currentSyncV2TerminalState());
  return () => { v2GenerationHandlers.delete(handler); };
}

export function _notifySyncV2Generation(state: SyncV2TerminalState | null): void {
  for (const handler of v2GenerationHandlers) handler(state);
}

/** Milliseconds since the current open Sync socket received any frame. */
export function _syncLinkIdleMs(): number {
  const link = liveSyncLink;
  return link && link.ws.readyState === WebSocket.OPEN && link.watchdog
    ? link.watchdog.idleMs()
    : Number.POSITIVE_INFINITY;
}

/** A dial in flight is already the redial, so resume must not close it. */
export function _syncLinkLiveness(): SyncLinkLiveness {
  const link = liveSyncLink;
  if (!link) return "none";
  return link.accepting && link.ws.readyState === WebSocket.OPEN ? "open" : "dialing";
}

export function _deactivateSyncV2Link(link: LiveSyncLink): void {
  if (!link.v2) return;
  link.v2 = null;
  if (liveSyncLink === link) _notifySyncV2Generation(null);
}

export function _cleanupSyncLink(link: LiveSyncLink): void {
  link.accepting = false;
  _deactivateSyncV2Link(link);
  clearTimeout(link.closeEscapeTimer ?? undefined);
  link.closeEscapeTimer = null;
  link.watchdog?.stop();
  clearTimeout(link.openTimer ?? undefined);
  link.openTimer = null;
  link.watchdog = null;
}

export function _armSyncCloseEscape(link: LiveSyncLink): void {
  clearTimeout(link.closeEscapeTimer ?? undefined);
  link.closeEscapeTimer = setTimeout(() => {
    link.accepting = false;
    _deactivateSyncV2Link(link);
    link.closeEscapeTimer = null;
    link.resolveClosed();
  }, WS_CLOSE_ESCAPE_MS);
}

export function _initiateSyncClose(reason: Exclude<SyncAbortReason, null>): void {
  const link = liveSyncLink;
  if (!link) return;
  link.accepting = false;
  _deactivateSyncV2Link(link);
  link.abortReason = reason;
  try { link.ws.close(); } catch { link.resolveClosed(); }
  _armSyncCloseEscape(link);
}

export function _closeFailedSyncLink(link: LiveSyncLink): void {
  link.accepting = false;
  _deactivateSyncV2Link(link);
  try { link.ws.close(); } catch { link.resolveClosed(); }
  _armSyncCloseEscape(link);
}
