// Debounced, cursor-paged terminal-content search for the current dashboard.
// One logical search owns one caller ID across pages, one in-flight RPC, and
// generation-fenced publication. GlobalSearchPage owns and disposes each instance.

import { createSignal, type Accessor } from "solid-js";
import { diag } from "@roost/shared/diag";
import {
  GlobalSearchPartialReason,
  type SessionsSearchGlobalMatch,
  type SessionsSearchGlobalPartial,
  type SessionsSearchGlobalResponse,
} from "@roost/shared/proto/coordinator_pb";
import {
  GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
  GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
  GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
  TERMINAL_SEARCH_ID_MAX_LENGTH,
  TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS,
} from "@roost/shared/terminal-search";
import { coordClient } from "../connect.ts";
import {
  captureDashboardResourceToken,
  isCurrentDashboardResourceToken,
  type DashboardResourceToken,
} from "../store/dashboard-selection.ts";
import { registerDashboardBoundContentSearch } from "./globalContentSearchRuntime.ts";
import {
  mergeGlobalContentSearchMatches,
  reconcileGlobalContentSearchPartials,
} from "./globalContentSearchResults.ts";

export const GLOBAL_CONTENT_SEARCH_DEBOUNCE_MS = 300;

interface GlobalContentSearchRequest {
  readonly query: string;
  readonly caseSensitive: boolean;
  readonly searchId: string;
  readonly cursor?: string;
  readonly maxSessions: number;
  readonly maxRowsPerSession: number;
  readonly maxMatches: number;
}

interface GlobalContentSearchRpc {
  sessionsSearchGlobal(
    request: GlobalContentSearchRequest,
    options?: { signal?: AbortSignal },
  ): Promise<SessionsSearchGlobalResponse>;
  sessionsCancelGlobalSearch(request: { searchId: string }): Promise<unknown>;
}

export interface GlobalContentSearchControllerDependencies {
  readonly rpc?: GlobalContentSearchRpc;
  readonly captureResourceToken?: () => DashboardResourceToken;
  readonly isResourceTokenCurrent?: (token: DashboardResourceToken) => boolean;
  readonly createSearchId?: () => string;
  readonly registerRuntime?: typeof registerDashboardBoundContentSearch;
  readonly schedule?: (callback: () => void, delayMs: number) => () => void;
  readonly recordCancelFailure?: (error: unknown) => void;
}

export interface GlobalContentSearchController {
  readonly matches: Accessor<readonly SessionsSearchGlobalMatch[]>;
  readonly partials: Accessor<readonly SessionsSearchGlobalPartial[]>;
  readonly nextCursor: Accessor<string | undefined>;
  readonly searchedSessions: Accessor<number>;
  readonly eligibleSessions: Accessor<number>;
  readonly truncated: Accessor<boolean>;
  readonly debouncing: Accessor<boolean>;
  readonly loading: Accessor<boolean>;
  readonly error: Accessor<string | null>;
  readonly retryable: Accessor<boolean>;
  readonly hasSearched: Accessor<boolean>;
  setSearch(query: string, caseSensitive: boolean): void;
  loadMore(): void;
  resumeAfterDashboardCutover(): void;
  retry(): void;
  resetForDashboardCutover(): void;
  dispose(): void;
}


interface ActiveSearchSpec {
  readonly query: string;
  readonly caseSensitive: boolean;
  readonly resourceToken: DashboardResourceToken;
}

interface ActivePageRequest {
  readonly controller: AbortController;
  readonly version: number;
}

const defaultRpc: GlobalContentSearchRpc = {
  sessionsSearchGlobal: (request, options) => coordClient.sessionsSearchGlobal(request, options),
  sessionsCancelGlobalSearch: (request) => coordClient.sessionsCancelGlobalSearch(request),
};


export function createGlobalContentSearchController(
  overrides: GlobalContentSearchControllerDependencies = {},
): GlobalContentSearchController {
  const rpc = overrides.rpc ?? defaultRpc;
  const captureResourceToken = overrides.captureResourceToken ?? captureDashboardResourceToken;
  const isResourceTokenCurrent = overrides.isResourceTokenCurrent ?? isCurrentDashboardResourceToken;
  const createSearchId = overrides.createSearchId ?? (() => crypto.randomUUID());
  const schedule = overrides.schedule ?? ((callback, delayMs) => {
    const timeout = setTimeout(callback, delayMs);
    return () => clearTimeout(timeout);
  });
  const recordCancelFailure = overrides.recordCancelFailure ?? ((error: unknown) => {
    diag("scrollback.global_search_cancel_failed", { error: String(error) });
  });
  const registerRuntime = overrides.registerRuntime ?? registerDashboardBoundContentSearch;

  const [matches, setMatches] = createSignal<readonly SessionsSearchGlobalMatch[]>([]);
  const [partials, setPartials] = createSignal<readonly SessionsSearchGlobalPartial[]>([]);
  const [nextCursor, setNextCursor] = createSignal<string | undefined>();
  const [searchedSessions, setSearchedSessions] = createSignal(0);
  const [eligibleSessions, setEligibleSessions] = createSignal(0);
  const [truncated, setTruncated] = createSignal(false);
  const [debouncing, setDebouncing] = createSignal(false);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [retryable, setRetryable] = createSignal(false);
  const [hasSearched, setHasSearched] = createSignal(false);

  let activeSpec: ActiveSearchSpec | null = null;
  let activeSearchId: string | null = null;
  let activePageRequest: ActivePageRequest | null = null;
  let cancelDebounce: (() => void) | null = null;
  let version = 0;
  let disposed = false;
  let suspendedResourceToken: DashboardResourceToken | null = null;
  let unregisterRuntime = () => {};
  let desiredSearch = { query: "", caseSensitive: false };

  function clearPublishedState(): void {
    setMatches([]);
    setPartials([]);
    setNextCursor(undefined);
    setSearchedSessions(0);
    setEligibleSessions(0);
    setTruncated(false);
    setDebouncing(false);
    setLoading(false);
    setError(null);
    setRetryable(false);
    setHasSearched(false);
  }

  function cancelCoordinatorSearch(searchId: string): void {
    void (async () => {
      try {
        await rpc.sessionsCancelGlobalSearch({ searchId });
      } catch (cancelError) {
        recordCancelFailure(cancelError);
      }
    })();
  }

  function stopLogicalSearch(): void {
    version++;
    cancelDebounce?.();
    cancelDebounce = null;
    activePageRequest?.controller.abort();
    activePageRequest = null;
    if (activeSearchId) cancelCoordinatorSearch(activeSearchId);
    activeSearchId = null;
    activeSpec = null;
    setDebouncing(false);
    setLoading(false);
  }

  function sameResourceToken(
    left: DashboardResourceToken,
    right: DashboardResourceToken,
  ): boolean {
    return left.generation === right.generation && left.dashboardId === right.dashboardId;
  }

  function requestIsCurrent(
    request: ActivePageRequest,
    spec: ActiveSearchSpec,
  ): boolean {
    return !disposed
      && request.version === version
      && activePageRequest === request
      && activeSpec === spec
      && !request.controller.signal.aborted
      && isResourceTokenCurrent(spec.resourceToken);
  }


  async function requestPage(cursor: string | undefined, append: boolean): Promise<void> {
    const spec = activeSpec;
    const searchId = activeSearchId;
    if (!spec || !searchId || activePageRequest || disposed) return;
    if (!isResourceTokenCurrent(spec.resourceToken)) {
      resetForDashboardCutover();
      return;
    }

    const request: ActivePageRequest = {
      controller: new AbortController(),
      version,
    };
    activePageRequest = request;
    setLoading(true);
    setError(null);
    try {
      const response = await rpc.sessionsSearchGlobal({
        query: spec.query,
        caseSensitive: spec.caseSensitive,
        searchId,
        ...(cursor === undefined ? {} : { cursor }),
        maxSessions: GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
        maxRowsPerSession: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
        maxMatches: GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
      }, { signal: request.controller.signal });
      if (!requestIsCurrent(request, spec)) return;

      const retiredEpochSessions = new Set(response.partials
        .filter((partial) => partial.reason === GlobalSearchPartialReason.EPOCH_CHANGED)
        .map((partial) => partial.sessionId));
      const currentPageMatches = response.matches.filter((match) =>
        !retiredEpochSessions.has(match.sessionId)
      );
      const retainedMatches = append
        ? matches().filter((match) => !retiredEpochSessions.has(match.sessionId))
        : [];
      setMatches(append
        ? mergeGlobalContentSearchMatches(retainedMatches, currentPageMatches)
        : currentPageMatches);
      setPartials(append
        ? reconcileGlobalContentSearchPartials(partials(), response.partials)
        : [...response.partials]);
      setNextCursor(response.nextCursor);
      setSearchedSessions(response.searchedSessions);
      setEligibleSessions(response.eligibleSessions);
      setTruncated(response.truncated);
      setHasSearched(true);
    } catch (searchError) {
      if (!requestIsCurrent(request, spec)) return;
      setError(searchError instanceof Error ? searchError.message : String(searchError));
      setRetryable(true);
      setHasSearched(true);
    } finally {
      if (activePageRequest === request) {
        activePageRequest = null;
        setLoading(false);
      }
    }
  }

  function beginFirstPage(expectedVersion: number): void {
    cancelDebounce = null;
    setDebouncing(false);
    if (disposed || expectedVersion !== version || !activeSpec) return;
    const searchId = createSearchId();
    if (!searchId || searchId.length > TERMINAL_SEARCH_ID_MAX_LENGTH) {
      setError("Unable to create a bounded search identifier.");
      setHasSearched(true);
      return;
    }
    activeSearchId = searchId;
    void requestPage(undefined, false);
  }

  function setSearch(query: string, caseSensitive: boolean): void {
    if (disposed) return;
    desiredSearch = { query, caseSensitive };
    const resourceToken = captureResourceToken();
    if (suspendedResourceToken && sameResourceToken(suspendedResourceToken, resourceToken)) return;
    suspendedResourceToken = null;
    if (
      activeSpec?.query === query
      && activeSpec.caseSensitive === caseSensitive
      && sameResourceToken(activeSpec.resourceToken, resourceToken)
    ) return;

    stopLogicalSearch();
    clearPublishedState();
    if (resourceToken.dashboardId === null) return;
    if (!query.trim()) return;
    if ([...query].length > TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS) {
      setError(`Terminal content queries are limited to ${TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS} characters.`);
      setHasSearched(true);
      return;
    }

    activeSpec = { query, caseSensitive, resourceToken };
    const expectedVersion = version;
    setDebouncing(true);
    cancelDebounce = schedule(
      () => beginFirstPage(expectedVersion),
      GLOBAL_CONTENT_SEARCH_DEBOUNCE_MS,
    );
  }

  function loadMore(): void {
    const cursor = nextCursor();
    if (!cursor || loading() || debouncing()) return;
    void requestPage(cursor, true);
  }

  function retry(): void {
    const spec = activeSpec;
    if (!spec || loading() || debouncing()) return;
    const { query, caseSensitive } = spec;
    activeSpec = null;
    setSearch(query, caseSensitive);
  }

  function resetForDashboardCutover(): void {
    stopLogicalSearch();
    clearPublishedState();
    suspendedResourceToken = captureResourceToken();
  }

  function resumeAfterDashboardCutover(): void {
    if (disposed || !suspendedResourceToken) return;
    suspendedResourceToken = null;
    activeSpec = null;
    setSearch(desiredSearch.query, desiredSearch.caseSensitive);
  }

  function dispose(): void {
    if (disposed) return;
    stopLogicalSearch();
    clearPublishedState();
    disposed = true;
    unregisterRuntime();
  }

  const controller: GlobalContentSearchController = {
    matches,
    partials,
    nextCursor,
    searchedSessions,
    eligibleSessions,
    truncated,
    debouncing,
    loading,
    error,
    retryable,
    hasSearched,
    setSearch,
    loadMore,
    resumeAfterDashboardCutover,
    retry,
    resetForDashboardCutover,
    dispose,
  };
  unregisterRuntime = registerRuntime(controller);
  return controller;
}
