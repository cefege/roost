// Normalizes caller-requested global-search page limits before cursor binding.
// The session handler uses this value for authorization breadth, worker work,
// result validation, and continuation identity so page semantics cannot drift.

import {
  GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
  GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
  GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
} from "@roost/shared/terminal-search";

export interface GlobalSearchPageLimits {
  readonly maxSessions: number;
  readonly maxRowsPerSession: number;
  readonly maxMatches: number;
}

function requestedOrMaximum(requested: number, maximum: number): number {
  return requested === 0 ? maximum : Math.min(requested, maximum);
}

export function normalizeGlobalSearchPageLimits(request: {
  readonly maxSessions: number;
  readonly maxRowsPerSession: number;
  readonly maxMatches: number;
}): GlobalSearchPageLimits {
  return {
    maxSessions: requestedOrMaximum(request.maxSessions, GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS),
    maxRowsPerSession: requestedOrMaximum(
      request.maxRowsPerSession,
      GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
    ),
    maxMatches: requestedOrMaximum(request.maxMatches, GLOBAL_TERMINAL_SEARCH_MAX_MATCHES),
  };
}
