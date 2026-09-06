// Owns worker scrollback-search cancellation identity and race handling.
// Browser command dispatch records explicit cancels here before scan admission.
// Bounded tombstones reject cancel-before-start reordering without lasting state.

import { TERMINAL_SEARCH_RPC_DEADLINE_MS } from "@roost/shared/terminal-search";
import type { ClientControlFrame } from "@roost/shared/wire";
import type { SessionManager } from "./session-manager.ts";

const SEARCH_CANCEL_TOMBSTONE_MAX = 128;
const SEARCH_CANCEL_TOMBSTONE_TTL_MS = TERMINAL_SEARCH_RPC_DEADLINE_MS * 2;

type SearchFrame = Extract<ClientControlFrame, { kind: "search-scrollback" }>;
type CancelSearchFrame = Extract<ClientControlFrame, { kind: "cancel-scrollback-search" }>;

export function searchOwnerKey(channelId: number, ownerId: string): string {
  return `${channelId}:${ownerId}`;
}

function searchCancellationKey(
  searchOwnerId: string,
  sessionId: string,
  searchId: string,
): string {
  return `${searchOwnerId}\u0000${sessionId}\u0000${searchId}`;
}

function pruneExpiredSearchCancellations(sessionMgr: SessionManager, nowMs: number): void {
  for (const [key, expiresAt] of sessionMgr.terminalSearchCancellations) {
    if (expiresAt > nowMs) continue;
    sessionMgr.terminalSearchCancellations.delete(key);
  }
}

function evictOldestSearchCancellation(sessionMgr: SessionManager): void {
  if (sessionMgr.terminalSearchCancellations.size < SEARCH_CANCEL_TOMBSTONE_MAX) return;
  const oldest = sessionMgr.terminalSearchCancellations.keys().next().value;
  if (oldest !== undefined) sessionMgr.terminalSearchCancellations.delete(oldest);
}

export function cancelSearchScrollback(
  frame: CancelSearchFrame,
  searchOwnerId: string,
  sessionMgr: SessionManager,
): void {
  const nowMs = Date.now();
  pruneExpiredSearchCancellations(sessionMgr, nowMs);
  evictOldestSearchCancellation(sessionMgr);
  sessionMgr.terminalSearchCancellations.set(
    searchCancellationKey(searchOwnerId, frame.session_id, frame.search_request_id),
    nowMs + SEARCH_CANCEL_TOMBSTONE_TTL_MS,
  );

  const session = sessionMgr.getBySessionId(frame.session_id);
  if (!session) return;
  const active = sessionMgr.terminalSearches.get(searchOwnerKey(session.channelId, searchOwnerId));
  if (active?.searchId === frame.search_request_id) active.controller.abort();
}

export function consumeSearchCancellation(
  frame: SearchFrame,
  searchOwnerId: string,
  sessionMgr: SessionManager,
): boolean {
  const nowMs = Date.now();
  pruneExpiredSearchCancellations(sessionMgr, nowMs);
  const key = searchCancellationKey(searchOwnerId, frame.session_id, frame.search_id);
  const wasCanceled = sessionMgr.terminalSearchCancellations.has(key);
  if (wasCanceled) sessionMgr.terminalSearchCancellations.delete(key);
  return wasCanceled;
}
