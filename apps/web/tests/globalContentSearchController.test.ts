// Terminal-content controller tests for debounce, cancellation, paging,
// resource-token fencing, and projection joins. RPC and timers are deterministic
// so stale completion behavior is observable without a browser or coordinator.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  GlobalSearchPartialReason,
  type SessionsSearchGlobalResponse,
} from "@roost/shared/proto/coordinator_pb";
import {
  GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
  GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
  GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
} from "@roost/shared/terminal-search";
import {
  createGlobalContentSearchController,
  type GlobalContentSearchController,
} from "../src/lib/globalContentSearchController.ts";
import { joinGlobalContentSearchMatches } from "../src/lib/globalContentSearchResults.ts";
import { _GlobalContentSearchRuntime } from "../src/lib/globalContentSearchRuntime.ts";
import type { NavigationSearchDocument } from "../src/store/navigation-search.ts";

interface CapturedRequest {
  readonly query: string;
  readonly caseSensitive: boolean;
  readonly searchId: string;
  readonly cursor?: string;
  readonly maxSessions: number;
  readonly maxRowsPerSession: number;
  readonly maxMatches: number;
}

interface CapturedOptions {
  readonly signal?: AbortSignal;
}

interface ReplyOptions {
  readonly sessionId?: string;
  readonly preview?: string;
  readonly cursor?: string;
  readonly truncated?: boolean;
  readonly partial?: GlobalSearchPartialReason;
  readonly searched?: number;
  readonly eligible?: number;
}

const controllers: GlobalContentSearchController[] = [];

function response(options: ReplyOptions = {}): SessionsSearchGlobalResponse {
  const sessionId = options.sessionId ?? "session-a";
  return {
    matches: options.preview === undefined ? [] : [{
      sessionId,
      row: 12n,
      col: 3,
      len: 5,
      preview: options.preview,
      gridEpoch: "grid-a:0",
    }],
    partials: options.partial === undefined ? [] : [{
      sessionId,
      reason: options.partial,
    }],
    nextCursor: options.cursor,
    searchedSessions: options.searched ?? 1,
    eligibleSessions: options.eligible ?? 1,
    truncated: options.truncated ?? false,
  } as SessionsSearchGlobalResponse;
}

function deferredResponse() {
  return Promise.withResolvers<SessionsSearchGlobalResponse>();
}

function harness() {
  const requests: CapturedRequest[] = [];
  const signals: AbortSignal[] = [];
  const cancellations: string[] = [];
  const cancelFailures: unknown[] = [];
  const scheduled = new Map<number, () => void>();
  let nextTimer = 1;
  let nextSearch = 1;
  let currentToken = { generation: 1 };
  let rpcImplementation = async (_request: CapturedRequest) => response();

  const controller = createGlobalContentSearchController({
    rpc: {
      sessionsSearchGlobal(request: CapturedRequest, options?: CapturedOptions) {
        requests.push(request);
        if (options?.signal) signals.push(options.signal);
        return rpcImplementation(request);
      },
      async sessionsCancelGlobalSearch(request: { searchId: string }) {
        cancellations.push(request.searchId);
        return {};
      },
    },
    captureResourceToken: () => ({ ...currentToken }),
    isResourceTokenCurrent: (token) => token.generation === currentToken.generation,
    createSearchId: () => `global-${nextSearch++}`,
    registerRuntime: () => () => {},
    schedule: (callback) => {
      const timer = nextTimer++;
      scheduled.set(timer, callback);
      return () => { scheduled.delete(timer); };
    },
    recordCancelFailure: (error) => { cancelFailures.push(error); },
  });
  controllers.push(controller);

  return {
    controller,
    requests,
    signals,
    cancellations,
    cancelFailures,
    setRpc(implementation: typeof rpcImplementation) {
      rpcImplementation = implementation;
    },
    setAuthGeneration(generation: number) {
      currentToken = { generation };
    },
    async fireDebounce() {
      const callbacks = [...scheduled.values()];
      scheduled.clear();
      for (const callback of callbacks) callback();
      await settle();
    },
  };
}

async function settle(): Promise<void> {
  for (let idx = 0; idx < 20; idx++) await Promise.resolve();
}

beforeEach(() => {
  controllers.length = 0;
});

afterEach(() => {
  for (const controller of controllers) controller.dispose();
});

describe("global terminal-content search controller", () => {
  test("debounces to the latest query and bounds caller search IDs", async () => {
    const search = harness();
    search.controller.setSearch("older", false);
    search.controller.setSearch("newest", true);
    expect(search.requests).toEqual([]);

    await search.fireDebounce();
    expect(search.requests).toHaveLength(1);
    expect(search.requests[0]).toMatchObject({
      query: "newest",
      caseSensitive: true,
      searchId: "global-1",
      maxSessions: GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
      maxRowsPerSession: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
      maxMatches: GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
    });
    expect(search.requests[0]!.searchId.length).toBeLessThanOrEqual(64);
  });

  test("does not offer retry for an overlong query", () => {
    const search = harness();
    search.controller.setSearch("x".repeat(257), false);
    expect(search.controller.error()).toContain("limited to 256");
    expect(search.controller.retryable()).toBe(false);
    search.controller.retry();
    expect(search.requests).toHaveLength(0);
  });

  test("aborts and explicitly cancels a replaced search while stale replies stay inert", async () => {
    const search = harness();
    const older = deferredResponse();
    const newer = deferredResponse();
    search.setRpc((request) => request.query === "older" ? older.promise : newer.promise);

    search.controller.setSearch("older", false);
    await search.fireDebounce();
    const olderSearchId = search.requests[0]!.searchId;
    search.controller.setSearch("newer", false);
    expect(search.signals[0]!.aborted).toBe(true);
    expect(search.cancellations).toEqual([olderSearchId]);

    await search.fireDebounce();
    newer.resolve(response({ preview: "new result" }));
    await settle();
    expect(search.controller.matches().map((match) => match.preview)).toEqual(["new result"]);

    older.resolve(response({ preview: "stale result" }));
    await settle();
    expect(search.controller.matches().map((match) => match.preview)).toEqual(["new result"]);

    const newerSearchId = search.requests[1]!.searchId;
    search.controller.dispose();
    expect(search.signals[1]!.aborted).toBe(false);
    expect(search.cancellations).toEqual([olderSearchId, newerSearchId]);
  });

  test("loads opaque cursors in order with one search ID and accumulated page results", async () => {
    const search = harness();
    search.setRpc(async (request) => {
      if (request.cursor === undefined) {
        return response({ preview: "page one", cursor: "opaque-1", searched: 2, eligible: 3 });
      }
      if (request.cursor === "opaque-1") {
        return response({
          sessionId: "session-b",
          preview: "page two",
          cursor: "opaque-2",
          partial: GlobalSearchPartialReason.DEADLINE,
          truncated: true,
          searched: 2,
          eligible: 3,
        });
      }
      return response({ sessionId: "session-c", preview: "page three", searched: 3, eligible: 3 });
    });

    search.controller.setSearch("needle", false);
    await search.fireDebounce();
    search.controller.loadMore();
    await settle();
    search.controller.loadMore();
    await settle();

    expect(search.requests.map((request) => request.cursor)).toEqual([
      undefined,
      "opaque-1",
      "opaque-2",
    ]);
    expect(new Set(search.requests.map((request) => request.searchId)).size).toBe(1);
    expect(search.controller.matches().map((match) => match.preview)).toEqual([
      "page one",
      "page two",
      "page three",
    ]);
    expect(search.controller.searchedSessions()).toBe(3);
    expect(search.controller.eligibleSessions()).toBe(3);
    expect(search.controller.partials()).toHaveLength(0);
    expect(search.controller.nextCursor()).toBeUndefined();
  });

  test("discards retired-epoch matches before accepting the restarted page", async () => {
    const search = harness();
    search.setRpc(async (request) => request.cursor === undefined
      ? response({
          preview: "retired grid",
          cursor: "epoch-restart",
          partial: GlobalSearchPartialReason.EPOCH_CHANGED,
          truncated: true,
        })
      : response({ preview: "current grid" }));

    search.controller.setSearch("epoch", false);
    await search.fireDebounce();
    expect(search.controller.matches()).toHaveLength(0);
    expect(search.controller.partials()[0]?.reason)
      .toBe(GlobalSearchPartialReason.EPOCH_CHANGED);
    search.controller.loadMore();
    await settle();
    expect(search.controller.matches().map((match) => match.preview))
      .toEqual(["current grid"]);
    expect(search.controller.partials()).toHaveLength(0);
  });

  test("deduplicates a repeated match identity across cursor pages", async () => {
    const search = harness();
    search.setRpc(async (request) => response({
      preview: "same row",
      cursor: request.cursor === undefined ? "opaque-repeat" : undefined,
    }));

    search.controller.setSearch("same", false);
    await search.fireDebounce();
    search.controller.loadMore();
    await settle();

    expect(search.controller.matches()).toHaveLength(1);
  });

  test("restarts with a new search ID after a cursor-page failure", async () => {
    const search = harness();
    search.setRpc(async (request) => {
      if (request.cursor) throw new Error("cursor expired");
      return response({ preview: "rebuilt", cursor: "single-use-cursor" });
    });
    search.controller.setSearch("restart", false);
    await search.fireDebounce();
    const firstSearchId = search.requests[0]!.searchId;
    search.controller.loadMore();
    await settle();
    expect(search.controller.error()).toBe("cursor expired");

    search.controller.retry();
    await search.fireDebounce();
    expect(search.requests.at(-1)?.cursor).toBeUndefined();
    expect(search.requests.at(-1)!.searchId).not.toBe(firstSearchId);
    expect(search.cancellations).toContain(firstSearchId);
    expect(search.controller.error()).toBeNull();
  });
  test("an auth-boundary reset cancels controllers and waits for a new resource token", async () => {
    const search = harness();
    const retiredCredential = deferredResponse();
    search.setRpc(() => retiredCredential.promise);
    search.controller.setSearch("marker", false);
    await search.fireDebounce();
    const searchId = search.requests[0]!.searchId;

    search.controller.resetForAuthBoundary();
    expect(search.signals[0]!.aborted).toBe(true);
    expect(search.cancellations).toEqual([searchId]);
    expect(search.controller.matches()).toEqual([]);

    search.controller.setSearch("marker", false);
    await search.fireDebounce();
    expect(search.requests).toHaveLength(1);

    search.setAuthGeneration(2);
    search.setRpc(async () => response({ preview: "next credential" }));
    search.controller.resumeAfterAuthBoundary();
    await search.fireDebounce();
    expect(search.requests).toHaveLength(2);
    expect(search.controller.matches().map((match) => match.preview)).toEqual(["next credential"]);
  });

  test("suspends controllers registered during an in-flight auth boundary", () => {
    const runtime = new _GlobalContentSearchRuntime();
    const transitions = { resets: 0, resumes: 0 };
    runtime.suspend();
    runtime.register({
      resetForAuthBoundary: () => { transitions.resets++; },
      resumeAfterAuthBoundary: () => { transitions.resumes++; },
    });
    expect(transitions).toEqual({ resets: 1, resumes: 0 });
    runtime.resume();
    expect(transitions).toEqual({ resets: 1, resumes: 1 });
  });

  test("drops a response after a resource-token boundary", async () => {
    const search = harness();
    const stale = deferredResponse();
    search.setRpc(() => stale.promise);
    search.controller.setSearch("retired needle", false);
    await search.fireDebounce();

    search.setAuthGeneration(2);
    stale.resolve(response({ preview: "foreign result" }));
    await settle();

    expect(search.controller.matches()).toEqual([]);
    expect(search.controller.hasSearched()).toBe(false);
    expect(search.controller.loading()).toBe(false);
  });

  test("joins matches only to current projection rows", () => {
    const known = {
      sessionId: "session-a",
      href: "/s/session-a",
      displayTitle: "Known terminal",
    } as NavigationSearchDocument;
    const matches = [
      ...response({ sessionId: "session-a", preview: "known" }).matches,
      ...response({ sessionId: "missing", preview: "stale" }).matches,
    ];

    const joined = joinGlobalContentSearchMatches(matches, [known]);
    expect(joined).toHaveLength(1);
    expect(joined[0]).toMatchObject({
      document: { sessionId: "session-a", href: "/s/session-a" },
      match: { sessionId: "session-a", preview: "known" },
    });
  });
});
