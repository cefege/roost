// Pure projection and page reconciliation for dashboard terminal-content search.
// The controller uses these functions to keep cursor pages identity-safe; the
// route joins matches only through its current dashboard metadata projection.

import {
  GlobalSearchPartialReason,
  type SessionsSearchGlobalMatch,
  type SessionsSearchGlobalPartial,
} from "@roost/shared/proto/coordinator_pb";
import type { NavigationSearchDocument } from "../store/navigation-search.ts";

export interface JoinedGlobalContentMatch {
  readonly match: SessionsSearchGlobalMatch;
  readonly document: NavigationSearchDocument;
}

const PARTIAL_REASON_LABELS: Record<GlobalSearchPartialReason, string> = {
  [GlobalSearchPartialReason.UNSPECIFIED]: "returned an unspecified incomplete result",
  [GlobalSearchPartialReason.WORKER_UNAVAILABLE]: "is unavailable",
  [GlobalSearchPartialReason.DEADLINE]: "did not finish before the page deadline",
  [GlobalSearchPartialReason.EPOCH_CHANGED]: "changed while its retained history was searched",
  [GlobalSearchPartialReason.MATCH_LIMIT]: "reached its match limit",
  [GlobalSearchPartialReason.HISTORY_EVICTED]: "has older history that is no longer retained",
  [GlobalSearchPartialReason.SESSION_CLOSED]: "closed during the search",
  [GlobalSearchPartialReason.MALFORMED_RESULT]: "returned an invalid search result",
};

const TERMINAL_PARTIAL_REASONS = new Set<GlobalSearchPartialReason>([
  GlobalSearchPartialReason.MATCH_LIMIT,
  GlobalSearchPartialReason.HISTORY_EVICTED,
  GlobalSearchPartialReason.SESSION_CLOSED,
]);

export function globalContentSearchPartialLabel(reason: GlobalSearchPartialReason): string {
  return PARTIAL_REASON_LABELS[reason] ?? PARTIAL_REASON_LABELS[GlobalSearchPartialReason.UNSPECIFIED];
}

/** Join only through the current scalar projection; a vanished row never gains a stale href. */
export function joinGlobalContentSearchMatches(
  matches: readonly SessionsSearchGlobalMatch[],
  documents: readonly NavigationSearchDocument[],
): readonly JoinedGlobalContentMatch[] {
  const documentsBySession = new Map(documents.map((document) => [document.sessionId, document]));
  const joined: JoinedGlobalContentMatch[] = [];
  for (const match of matches) {
    const document = documentsBySession.get(match.sessionId);
    if (document) joined.push({ match, document });
  }
  return joined;
}

export function mergeGlobalContentSearchMatches(
  current: readonly SessionsSearchGlobalMatch[],
  incoming: readonly SessionsSearchGlobalMatch[],
): readonly SessionsSearchGlobalMatch[] {
  const merged = [...current];
  const identities = new Set(current.map((match) =>
    `${match.sessionId}\u0000${match.gridEpoch}\u0000${match.row}:${match.col}:${match.len}`
  ));
  for (const match of incoming) {
    const identity = `${match.sessionId}\u0000${match.gridEpoch}\u0000${match.row}:${match.col}:${match.len}`;
    if (identities.has(identity)) continue;
    identities.add(identity);
    merged.push(match);
  }
  return merged;
}

/** Successful continuation replaces retryable partials while retaining final floors/caps. */
export function reconcileGlobalContentSearchPartials(
  current: readonly SessionsSearchGlobalPartial[],
  incoming: readonly SessionsSearchGlobalPartial[],
): readonly SessionsSearchGlobalPartial[] {
  const retained = current.filter((partial) => TERMINAL_PARTIAL_REASONS.has(partial.reason));
  const merged = [...retained];
  const identities = new Set(retained.map((partial) =>
    `${partial.sessionId}\u0000${partial.reason}`
  ));
  for (const partial of incoming) {
    const identity = `${partial.sessionId}\u0000${partial.reason}`;
    if (identities.has(identity)) continue;
    identities.add(identity);
    merged.push(partial);
  }
  return merged;
}
