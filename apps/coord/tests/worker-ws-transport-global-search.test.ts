// Exercises coordinator-authorized global terminal search through fake routable
// worker handles and the real pending-RPC table. It pins dashboard isolation,
// fan-out caps, response partials, and malformed worker rejection without
// depending on worker implementation details. Sibling
// worker-ws-transport-global-search-cursor.test.ts owns cursor continuation.

import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  GlobalSearchPartialReason,
  SessionsSearchGlobalRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
  GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
  GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS,
  GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
  type WorkerGlobalSearchEntry,
} from "@roost/shared/terminal-search";
import { _pendingRpcStats } from "../src/router/pending-rpcs.ts";
import {
  GLOBAL_TEST_WORKER_A1,
  GLOBAL_TEST_WORKER_A2,
  globalSearchOkEntry as okEntry,
  globalSearchSessionId as sessionId,
  startGlobalSearchTestFixture,
  type GlobalSearchTestFixture,
} from "./global-search-test-fixture.ts";

let fixture: GlobalSearchTestFixture;
beforeAll(async () => { fixture = await startGlobalSearchTestFixture(); });
beforeEach(async () => { await fixture.reset(); });
afterAll(async () => { await fixture.cleanup(); });

describe("authorized global scrollback fan-out", () => {
  test("enumerates only the newest 32 authorized open sessions and sends one capped batch per worker", async () => {
    const workerA1 = fixture.installWorker(GLOBAL_TEST_WORKER_A1);
    const workerA2 = fixture.installWorker(GLOBAL_TEST_WORKER_A2);
    for (let index = 1; index <= 35; index++) {
      await fixture.insertSession({
        id: sessionId(index),
        workerFp: index === 35 ? GLOBAL_TEST_WORKER_A2 : GLOBAL_TEST_WORKER_A1,
        createdAt: index,
      });
    }
    await fixture.insertSession({
      id: sessionId(101),
      workerFp: GLOBAL_TEST_WORKER_A1,
      status: "closed",
      createdAt: 101,
    });

    const responsePromise = fixture.handlers().sessionsSearchGlobal(
      create(SessionsSearchGlobalRequestSchema, {
        query: "needle",
        caseSensitive: true,
        searchId: "fanout-caps",
        maxSessions: GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS + 100,
        maxRowsPerSession: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION + 100,
        maxMatches: GLOBAL_TERMINAL_SEARCH_MAX_MATCHES + 100,
      }),
      fixture.context(),
    );
    const [commandA1] = await workerA1.waitForKind("search-scrollback-batch");
    const [commandA2] = await workerA2.waitForKind("search-scrollback-batch");
    const commands = [commandA1!, commandA2!];
    const requestedIds = commands.flatMap((command) => {
      expect(command.browserCommand.browserId).toBe("global-browser:global-tab");
      expect(command.control).toMatchObject({
        kind: "search-scrollback-batch",
        search_id: "fanout-caps",
        query: "needle",
        case_sensitive: true,
        max_rows_per_session: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
        deadline_ms: GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS,
      });
      expect(command.control).not.toHaveProperty("regex");
      const sessions = command.control.sessions as Array<Record<string, unknown>>;
      expect(sessions.every((session) => session.grid_epoch === "")).toBe(true);
      expect(sessions.every((session) => !("before_row" in session))).toBe(true);
      return sessions.map((session) => session.session_id as string);
    });
    expect(requestedIds).toHaveLength(GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS);
    expect(new Set(requestedIds).size).toBe(GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS);
    expect(requestedIds).not.toContain(sessionId(1));
    expect(requestedIds).not.toContain(sessionId(2));
    expect(requestedIds).not.toContain(sessionId(3));
    expect(requestedIds).not.toContain(sessionId(101));
    for (const command of commands) {
      const sessionCount = (command.control.sessions as unknown[]).length;
      expect(command.control.max_matches).toBe(sessionCount * 8);
    }
    expect(commands.reduce(
      (sum, command) => sum + (command.control.max_matches as number),
      0,
    )).toBe(GLOBAL_TERMINAL_SEARCH_MAX_MATCHES);

    for (const command of commands) {
      const ids = (command.control.sessions as Array<{ session_id: string }>)
        .map((session) => session.session_id);
      const worker = command.workerFp === GLOBAL_TEST_WORKER_A1 ? workerA1 : workerA2;
      worker.respond(command, { entries: ids.map((id) => okEntry(id)) });
    }
    const response = await responsePromise;
    expect(response.eligibleSessions).toBe(35);
    expect(response.searchedSessions).toBe(GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS);
    expect(response.matches).toHaveLength(GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS);
    expect(response.partials).toHaveLength(0);
    expect(response.nextCursor).toBeUndefined();
    // 35 sessions are authorized and 32 fit one page: the browser must be told
    // coverage is partial rather than "32 of 32 searched".
    expect(response.truncated).toBe(true);
    expect(_pendingRpcStats().pending).toBe(0);
  });

  test("defers zero-budget workers and preserves cumulative progress", async () => {
    const firstWorker = fixture.installWorker(GLOBAL_TEST_WORKER_A1);
    const deferredWorker = fixture.installWorker(GLOBAL_TEST_WORKER_A2);
    await fixture.insertSession({
      id: sessionId(121),
      workerFp: GLOBAL_TEST_WORKER_A2,
      createdAt: 1,
    });
    await fixture.insertSession({
      id: sessionId(122),
      workerFp: GLOBAL_TEST_WORKER_A1,
      createdAt: 2,
    });
    const handlers = fixture.handlers();
    const request = {
      query: "needle",
      searchId: "caller-limits",
      maxSessions: 2,
      maxRowsPerSession: 17,
      maxMatches: 1,
    };
    const firstPromise = handlers.sessionsSearchGlobal(
      create(SessionsSearchGlobalRequestSchema, request),
      fixture.context(),
    );
    const [firstCommand] = await firstWorker.waitForKind("search-scrollback-batch");
    expect(firstCommand!.control.max_rows_per_session).toBe(17);
    expect(firstCommand!.control.max_matches).toBe(1);
    expect(deferredWorker.commands).toHaveLength(0);
    const firstId = (firstCommand!.control.sessions as Array<{ session_id: string }>)[0]!.session_id;
    firstWorker.respond(firstCommand!, { entries: [okEntry(firstId, "epoch-first", {
      matches: [{ row: 20, col: 1, len: 6, preview: "needle" }],
      scrollback_total: 30,
      scanned_start_row: 13,
      scanned_end_row: 30,
      next_before_row: 13,
      stop_reason: "row_limit",
    })] });
    const first = await firstPromise;
    expect(first.eligibleSessions).toBe(2);
    expect(first.searchedSessions).toBe(1);
    expect(first.nextCursor).toBeDefined();

    const secondPromise = handlers.sessionsSearchGlobal(
      create(SessionsSearchGlobalRequestSchema, {
        ...request,
        cursor: first.nextCursor,
      }),
      fixture.context(),
    );
    const [secondCommand] = await deferredWorker.waitForKind("search-scrollback-batch");
    expect(secondCommand!.control.max_matches).toBe(1);
    expect(firstWorker.commands).toHaveLength(1);
    const secondId = (secondCommand!.control.sessions as Array<{ session_id: string }>)[0]!.session_id;
    deferredWorker.respond(secondCommand!, { entries: [okEntry(secondId)] });
    const second = await secondPromise;
    expect(second.eligibleSessions).toBe(2);
    expect(second.searchedSessions).toBe(2);
    expect(second.nextCursor).toBeDefined();
  });

  test("returns every typed per-session failure without turning the page into empty success", async () => {
    const worker = fixture.installWorker(GLOBAL_TEST_WORKER_A1);
    const cases = [
      "deadline",
      "epoch",
      "match",
      "history",
      "closed",
      "internal",
    ] as const;
    for (let index = 0; index < cases.length; index++) {
      await fixture.insertSession({
        id: sessionId(200 + index),
        workerFp: GLOBAL_TEST_WORKER_A1,
        createdAt: index,
      });
    }
    const unavailableId = sessionId(299);
    await fixture.insertSession({
      id: unavailableId,
      workerFp: GLOBAL_TEST_WORKER_A2,
      createdAt: 99,
    });
    const responsePromise = fixture.handlers().sessionsSearchGlobal(
      create(SessionsSearchGlobalRequestSchema, {
        query: "partial",
        searchId: "partial-families",
      }),
      fixture.context(),
    );
    const [command] = await worker.waitForKind("search-scrollback-batch");
    const ids = (command!.control.sessions as Array<{ session_id: string }>)
      .map((session) => session.session_id);
    const entries: WorkerGlobalSearchEntry[] = ids.map((id) => {
      const sequence = Number(id.slice(-3));
      if (sequence === 200) {
        return okEntry(id, "deadline-epoch", {
          matches: [],
          truncated: true,
          scanned_start_row: 4,
          scanned_end_row: 10,
          stop_reason: "deadline",
        });
      }
      if (sequence === 201) {
        return { status: "error", session_id: id, error: "epoch_changed" };
      }
      if (sequence === 202) {
        return okEntry(id, "match-epoch", {
          truncated: true,
          scanned_start_row: 5,
          stop_reason: "match_limit",
        });
      }
      if (sequence === 203) {
        return okEntry(id, "floor-epoch", {
          scanned_start_row: 5,
          history_floor: "evicted",
        });
      }
      if (sequence === 204) return { status: "error", session_id: id, error: "session_closed" };
      return { status: "error", session_id: id, error: "internal" };
    });
    worker.respond(command!, { entries });
    const response = await responsePromise;
    const reasonBySession = new Map(
      response.partials!.map((partial) => [partial.sessionId, partial.reason]),
    );
    expect(new Set(reasonBySession.values())).toEqual(new Set([
      GlobalSearchPartialReason.WORKER_UNAVAILABLE,
      GlobalSearchPartialReason.DEADLINE,
      GlobalSearchPartialReason.EPOCH_CHANGED,
      GlobalSearchPartialReason.MATCH_LIMIT,
      GlobalSearchPartialReason.HISTORY_EVICTED,
      GlobalSearchPartialReason.SESSION_CLOSED,
      GlobalSearchPartialReason.MALFORMED_RESULT,
    ]));
    expect(reasonBySession.get(unavailableId))
      .toBe(GlobalSearchPartialReason.WORKER_UNAVAILABLE);
    expect(response.truncated).toBe(true);
    expect(response.nextCursor).toBeDefined();
    expect(response.eligibleSessions).toBe(7);
    expect(response.searchedSessions).toBe(6);
    expect(response.matches!.length).toBeGreaterThan(0);
  });
});
