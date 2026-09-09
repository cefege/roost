// Owns bounded, browser-bound continuation state and cancellation ordering for
// install-wide terminal search. Session handlers create one owner per factory
// and inject it into the global-search handler; no state escapes that composition.
// Expiry and oldest-device eviction are lazy so cursors require no background timer.

import { randomUUID } from "node:crypto";
import {
  GLOBAL_TERMINAL_SEARCH_CURSOR_TTL_MS,
  GLOBAL_TERMINAL_SEARCH_MAX_CURSORS_PER_DEVICE,
} from "@roost/shared/terminal-search";

export const _GLOBAL_SEARCH_MAX_CANCEL_TOMBSTONES_PER_DEVICE = 128;
const GLOBAL_SEARCH_MAX_CANCEL_TOMBSTONES = 4_096;
export const _GLOBAL_SEARCH_MAX_ACTIVE_PER_DEVICE = 4;
const GLOBAL_SEARCH_MAX_ACTIVE = 1_024;
export interface GlobalSearchIdentity {
  deviceFingerprint: string;
  tabId: string;
  searchId: string;
}
export type GlobalSearchAdmission = "started" | "cancelled" | "duplicate" | "capacity";

export interface GlobalSearchCursorBinding extends GlobalSearchIdentity {
  query: string;
  caseSensitive: boolean;
  maxSessions: number;
  maxRowsPerSession: number;
  maxMatches: number;
}

export interface GlobalSearchSessionPosition {
  sessionId: string;
  workerFp: string;
  gridEpoch: string;
  beforeRow?: number;
}

export interface GlobalSearchCursorProgress {
  sessions: readonly GlobalSearchSessionPosition[];
  eligibleSessions: number;
  searchedSessionIds: readonly string[];
}

export interface GlobalSearchContinuation {
  /** Where the next page resumes for this session. */
  position: GlobalSearchSessionPosition;
  /** Whether this page consumed a worker page for the session. A consumed page
   *  MUST advance the row cursor; a session the page never reached (contended
   *  lane, offline worker) legitimately retries its own position. */
  searched: boolean;
  /** The `beforeRow` this page requested for the session, absent on a session
   *  no page has scanned yet. */
  requestedBeforeRow?: number;
}

export interface GlobalSearchCursorIssue {
  binding: GlobalSearchCursorBinding;
  continuations: readonly GlobalSearchContinuation[];
  eligibleSessions: number;
  searchedSessionIds: readonly string[];
}

interface CursorRecord {
  token: string;
  binding: GlobalSearchCursorBinding;
  sessions: readonly GlobalSearchSessionPosition[];
  eligibleSessions: number;
  searchedSessionIds: readonly string[];
  createdOrder: number;
  expiresAtMs: number;
}

interface ActiveSearchRecord {
  identity: GlobalSearchIdentity;
  selectedSessions: readonly GlobalSearchSessionPosition[];
  cancellationListeners: Set<() => void>;
}

interface CancellationTombstone {
  deviceFingerprint: string;
  expiresAtMs: number;
}

export interface GlobalSearchCursorOwnerOptions {
  now?: () => number;
  newToken?: () => string;
}


function identityKey(identity: GlobalSearchIdentity): string {
  return JSON.stringify([
    identity.deviceFingerprint,
    identity.tabId,
    identity.searchId,
  ]);
}

function sameIdentity(
  left: GlobalSearchIdentity,
  right: GlobalSearchIdentity,
): boolean {
  return left.deviceFingerprint === right.deviceFingerprint
    && left.tabId === right.tabId
    && left.searchId === right.searchId;
}

function sameBinding(
  left: GlobalSearchCursorBinding,
  right: GlobalSearchCursorBinding,
): boolean {
  return sameIdentity(left, right)
    && left.query === right.query
    && left.caseSensitive === right.caseSensitive
    && left.maxSessions === right.maxSessions
    && left.maxRowsPerSession === right.maxRowsPerSession
    && left.maxMatches === right.maxMatches;
}

export interface GlobalSearchCancellationPreparation {
  shouldDispatch: boolean;
  selectedSessions: readonly GlobalSearchSessionPosition[];
}

/** Per-router owner for continuation cursors, active selections, and cancel tombstones. */
export class GlobalSearchCursorOwner {
  readonly #now: () => number;
  readonly #newToken: () => string;
  readonly #cursors = new Map<string, CursorRecord>();
  readonly #activeSearches = new Map<string, ActiveSearchRecord>();
  readonly #cancelledSearches = new Map<string, CancellationTombstone>();
  #createdOrder = 0;

  constructor(options: GlobalSearchCursorOwnerOptions = {}) {
    this.#now = options.now ?? Date.now;
    this.#newToken = options.newToken ?? randomUUID;
  }

  beginSearch(identity: GlobalSearchIdentity): GlobalSearchAdmission {
    this.#purgeExpired();
    const key = identityKey(identity);
    if (this.#cancelledSearches.has(key)) return "cancelled";
    if (this.#activeSearches.has(key)) return "duplicate";
    let activeForDevice = 0;
    for (const active of this.#activeSearches.values()) {
      if (active.identity.deviceFingerprint === identity.deviceFingerprint) {
        activeForDevice++;
      }
    }
    if (
      activeForDevice >= _GLOBAL_SEARCH_MAX_ACTIVE_PER_DEVICE
      || this.#activeSearches.size >= GLOBAL_SEARCH_MAX_ACTIVE
    ) return "capacity";
    this.#activeSearches.set(key, {
      identity: { ...identity },
      selectedSessions: [],
      cancellationListeners: new Set(),
    });
    return "started";
  }

  selectSessions(
    identity: GlobalSearchIdentity,
    sessions: readonly GlobalSearchSessionPosition[],
  ): boolean {
    this.#purgeExpired();
    const key = identityKey(identity);
    const active = this.#activeSearches.get(key);
    if (
      this.#cancelledSearches.has(key)
      || !active
      || !sameIdentity(active.identity, identity)
    ) return false;
    active.selectedSessions = sessions.map((session) => ({ ...session }));
    return true;
  }

  onCancel(identity: GlobalSearchIdentity, listener: () => void): () => void {
    const active = this.#activeSearches.get(identityKey(identity));
    if (!active || !sameIdentity(active.identity, identity)) {
      listener();
      return () => {};
    }
    active.cancellationListeners.add(listener);
    return () => active.cancellationListeners.delete(listener);
  }

  isCancelled(identity: GlobalSearchIdentity): boolean {
    this.#purgeExpired();
    return this.#cancelledSearches.has(identityKey(identity));
  }

  finishSearch(identity: GlobalSearchIdentity): void {
    const key = identityKey(identity);
    const active = this.#activeSearches.get(key);
    if (active && sameIdentity(active.identity, identity)) {
      this.#activeSearches.delete(key);
    }
  }

  issueCursor(issue: GlobalSearchCursorIssue): string {
    this.#purgeExpired();
    const { binding, continuations, eligibleSessions, searchedSessionIds } = issue;
    if (
      continuations.length === 0
      || continuations.length > binding.maxSessions
      || eligibleSessions < continuations.length
      || searchedSessionIds.length > eligibleSessions
    ) {
      throw new Error("global search cursor requires bounded progress");
    }
    const seenSessionIds = new Set<string>();
    for (const { position, searched, requestedBeforeRow } of continuations) {
      if (position.gridEpoch.length === 0 && position.beforeRow !== undefined) {
        throw new Error("global search row continuation requires a grid epoch");
      }
      if (seenSessionIds.has(position.sessionId)) {
        throw new Error("global search continuation sessions must be unique");
      }
      seenSessionIds.add(position.sessionId);
      // A page that actually scanned a session must leave it strictly closer to
      // the history floor. An unchanged row would page over the same rows
      // forever, holding a worker lane per page and never finishing.
      if (
        searched
        && requestedBeforeRow !== undefined
        && position.beforeRow !== undefined
        && position.beforeRow >= requestedBeforeRow
      ) {
        throw new Error("global search continuation must advance a searched session");
      }
    }
    this.#evictOldestDeviceCursor(binding.deviceFingerprint);
    const token = this.#newToken();
    const now = this.#now();
    this.#cursors.set(token, {
      token,
      binding: { ...binding },
      sessions: continuations.map(({ position }) => ({ ...position })),
      eligibleSessions,
      searchedSessionIds: [...searchedSessionIds],
      createdOrder: ++this.#createdOrder,
      expiresAtMs: now + GLOBAL_TERMINAL_SEARCH_CURSOR_TTL_MS,
    });
    return token;
  }

  claimCursor(
    token: string,
    binding: GlobalSearchCursorBinding,
  ): GlobalSearchCursorProgress | null {
    this.#purgeExpired();
    const cursor = this.#cursors.get(token);
    if (!cursor || !sameBinding(cursor.binding, binding)) return null;
    this.#cursors.delete(token);
    return {
      sessions: cursor.sessions.map((session) => ({ ...session })),
      eligibleSessions: cursor.eligibleSessions,
      searchedSessionIds: [...cursor.searchedSessionIds],
    };
  }

  prepareCancellation(
    identity: GlobalSearchIdentity,
  ): GlobalSearchCancellationPreparation {
    this.#purgeExpired();
    const key = identityKey(identity);
    if (this.#cancelledSearches.has(key)) {
      return { shouldDispatch: false, selectedSessions: [] };
    }
    const selected = new Map<string, GlobalSearchSessionPosition>();
    const active = this.#activeSearches.get(key);
    if (active && sameIdentity(active.identity, identity)) {
      for (const session of active.selectedSessions) {
        selected.set(session.sessionId, session);
      }
    }
    for (const [token, cursor] of this.#cursors) {
      if (sameIdentity(cursor.binding, identity)) this.#cursors.delete(token);
    }
    this.#evictCancellationCapacity(identity.deviceFingerprint);
    this.#cancelledSearches.set(key, {
      deviceFingerprint: identity.deviceFingerprint,
      expiresAtMs: this.#now() + GLOBAL_TERMINAL_SEARCH_CURSOR_TTL_MS,
    });
    return { shouldDispatch: true, selectedSessions: [...selected.values()] };
  }

  completeCancellation(identity: GlobalSearchIdentity): void {
    const key = identityKey(identity);
    const active = this.#activeSearches.get(key);
    if (!active || !sameIdentity(active.identity, identity)) return;
    this.#activeSearches.delete(key);
    for (const listener of active.cancellationListeners) listener();
  }

  #purgeExpired(): void {
    const now = this.#now();
    for (const [token, cursor] of this.#cursors) {
      if (cursor.expiresAtMs <= now) this.#cursors.delete(token);
    }
    for (const [key, tombstone] of this.#cancelledSearches) {
      if (tombstone.expiresAtMs <= now) this.#cancelledSearches.delete(key);
    }
  }

  #evictOldestDeviceCursor(deviceFingerprint: string): void {
    const deviceCursors = [...this.#cursors.values()]
      .filter((cursor) => cursor.binding.deviceFingerprint === deviceFingerprint)
      .sort((left, right) => left.createdOrder - right.createdOrder);
    while (deviceCursors.length >= GLOBAL_TERMINAL_SEARCH_MAX_CURSORS_PER_DEVICE) {
      const oldest = deviceCursors.shift();
      if (oldest) this.#cursors.delete(oldest.token);
    }
  }

  #evictCancellationCapacity(deviceFingerprint: string): void {
    let deviceCount = 0;
    for (const tombstone of this.#cancelledSearches.values()) {
      if (tombstone.deviceFingerprint === deviceFingerprint) deviceCount++;
    }
    while (deviceCount >= _GLOBAL_SEARCH_MAX_CANCEL_TOMBSTONES_PER_DEVICE) {
      const oldest = [...this.#cancelledSearches]
        .find(([, tombstone]) => tombstone.deviceFingerprint === deviceFingerprint);
      if (!oldest) break;
      this.#cancelledSearches.delete(oldest[0]);
      deviceCount--;
    }
    while (this.#cancelledSearches.size >= GLOBAL_SEARCH_MAX_CANCEL_TOMBSTONES) {
      const oldestKey = this.#cancelledSearches.keys().next().value;
      if (oldestKey === undefined) break;
      this.#cancelledSearches.delete(oldestKey);
    }
  }

}

