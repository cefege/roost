// Pins continuation epoch semantics, strict worker-batch validation, cancel
// ordering/grouping, per-worker serialization, and pending cleanup for global
// terminal search. It uses the focused fake transport fixture and production
// cursor/lane owners so races exercise the same coordinator state transitions.

import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  GlobalSearchPartialReason,
  SessionsCancelGlobalSearchRequestSchema,
  SessionsSearchGlobalRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
  GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
  GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
  type WorkerSearchScrollbackMatch,
  type WorkerSearchScrollbackResult,
} from "@roost/shared/terminal-search";
import { GlobalSearchCursorOwner } from "../src/connect/global-search-cursors.ts";
import { GlobalSearchWorkerLaneOwner } from "../src/connect/global-search-worker-lanes.ts";
import { _pendingRpcStats } from "../src/router/pending-rpcs.ts";
import {
  GLOBAL_TEST_WORKER_A1,
  GLOBAL_TEST_WORKER_A2,
  startGlobalSearchTestFixture,
  type GlobalSearchTestFixture,
} from "./global-search-test-fixture.ts";

function sessionId(sequence: number): string {
  return `00000000-0000-4000-8000-${String(sequence).padStart(12, "0")}`;
}

function workerResult(
  gridEpoch: string,
  overrides: Partial<WorkerSearchScrollbackResult> = {},
): WorkerSearchScrollbackResult {
  return {
    matches: [],
    truncated: false,
    scrollback_total: 3_000,
    cols: 80,
    grid_epoch: gridEpoch,
    scanned_start_row: 0,
    scanned_end_row: 10,
    history_floor: "none",
    stop_reason: "complete",
    ...overrides,
  };
}

async function connectError(result: unknown): Promise<ConnectError> {
  try {
    await result;
  } catch (error) {
    expect(error).toBeInstanceOf(ConnectError);
    return error as ConnectError;
  }
  throw new Error("expected ConnectError");
}


let fixture: GlobalSearchTestFixture;
beforeAll(async () => { fixture = await startGlobalSearchTestFixture(); });
beforeEach(async () => { await fixture.reset(); });
afterAll(async () => { await fixture.cleanup(); });

describe("global search continuation and result validation", () => {
  test("sends the exact returned epoch and exclusive row, then resets only on epoch change", async () => {
    const worker = fixture.installWorker(GLOBAL_TEST_WORKER_A1);
    const id = sessionId(400);
    await fixture.insertSession({ id, workerFp: GLOBAL_TEST_WORKER_A1 });
    const owner = new GlobalSearchCursorOwner();
    const lanes = new GlobalSearchWorkerLaneOwner();
    const handlers = fixture.handlers(owner, lanes);
    const request = {
      query: "needle",
      searchId: "epoch-pages",
      caseSensitive: true,
    };

    const firstPromise = handlers.sessionsSearchGlobal(
      create(SessionsSearchGlobalRequestSchema, request),
      fixture.context(),
    );
    const [first] = await worker.waitForKind("search-scrollback-batch", 1);
    worker.respond(first!, { entries: [{
      status: "ok",
      session_id: id,
      result: workerResult("epoch-one", {
        scanned_start_row: 100,
        scanned_end_row: 100 + GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
        next_before_row: 100,
        stop_reason: "row_limit",
      }),
    }] });
    const firstResponse = await firstPromise;
    expect(firstResponse.nextCursor).toBeDefined();

    const secondPromise = handlers.sessionsSearchGlobal(
      create(SessionsSearchGlobalRequestSchema, {
        ...request,
        cursor: firstResponse.nextCursor,
      }),
      fixture.context(),
    );
    const [, second] = await worker.waitForKind("search-scrollback-batch", 2);
    expect(second!.control.sessions).toEqual([{
      session_id: id,
      grid_epoch: "epoch-one",
      before_row: 100,
    }]);
    worker.respond(second!, { entries: [{
      status: "ok",
      session_id: id,
      result: workerResult("epoch-two", {
        truncated: true,
        scanned_start_row: 100,
        scanned_end_row: 100,
        stop_reason: "deadline",
      }),
    }] });
    const secondResponse = await secondPromise;
    expect(secondResponse.partials![0]?.reason)
      .toBe(GlobalSearchPartialReason.DEADLINE);
    expect(secondResponse.nextCursor).toBeDefined();

    const thirdPromise = handlers.sessionsSearchGlobal(
      create(SessionsSearchGlobalRequestSchema, {
        ...request,
        cursor: secondResponse.nextCursor,
      }),
      fixture.context(),
    );
    const [, , third] = await worker.waitForKind("search-scrollback-batch", 3);
    expect(third!.control.sessions).toEqual([{
      session_id: id,
      grid_epoch: "epoch-two",
    }]);
    worker.respond(third!, { entries: [{
      status: "ok",
      session_id: id,
      result: workerResult("epoch-two"),
    }] });
    expect((await thirdPromise).nextCursor).toBeUndefined();
  });

  test("turns missing, duplicate, reordered, over-budget, row, and epoch lies into malformed partials", async () => {
    const worker = fixture.installWorker(GLOBAL_TEST_WORKER_A1);
    const firstId = sessionId(410);
    const secondId = sessionId(411);
    await fixture.insertSession({ id: firstId, workerFp: GLOBAL_TEST_WORKER_A1, createdAt: 1 });
    await fixture.insertSession({ id: secondId, workerFp: GLOBAL_TEST_WORKER_A1, createdAt: 2 });
    const owner = new GlobalSearchCursorOwner();
    const handlers = fixture.handlers(owner, new GlobalSearchWorkerLaneOwner());
    const largeMatches = (count: number): WorkerSearchScrollbackMatch[] =>
      Array.from({ length: count }, (_, index) => ({
        row: 5,
        col: index,
        len: 1,
        preview: "x",
      }));
    const malformedResults: unknown[] = [
      { entries: [] },
      { entries: [
        { status: "ok", session_id: secondId, result: workerResult("epoch") },
        { status: "ok", session_id: secondId, result: workerResult("epoch") },
      ] },
      { entries: [
        { status: "ok", session_id: firstId, result: workerResult("epoch") },
        { status: "ok", session_id: secondId, result: workerResult("epoch") },
      ] },
      { entries: [
        {
          status: "ok",
          session_id: secondId,
          result: workerResult("epoch", {
            scanned_start_row: 1,
            scanned_end_row: 1 + GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION + 1,
          }),
        },
        { status: "ok", session_id: firstId, result: workerResult("epoch") },
      ] },
      { entries: [
        {
          status: "ok",
          session_id: secondId,
          result: workerResult("epoch", { matches: largeMatches(200) }),
        },
        {
          status: "ok",
          session_id: firstId,
          result: workerResult("epoch", { matches: largeMatches(56) }),
        },
      ] },
    ];
    for (let index = 0; index < malformedResults.length; index++) {
      const responsePromise = handlers.sessionsSearchGlobal(
        create(SessionsSearchGlobalRequestSchema, {
          query: "malformed",
          searchId: `malformed-${index}`,
        }),
        fixture.context(),
      );
      const commands = await worker.waitForKind("search-scrollback-batch", index + 1);
      worker.respond(commands[index]!, malformedResults[index]);
      const response = await responsePromise;
      expect(response.matches).toHaveLength(0);
      expect(response.searchedSessions).toBe(0);
      expect(response.partials).toHaveLength(2);
      expect(response.partials!.every((partial) =>
        partial.reason === GlobalSearchPartialReason.MALFORMED_RESULT
      )).toBe(true);
    }

    const cursor = owner.issueCursor({
      binding: {
        deviceFingerprint: "global-browser",
        tabId: "global-tab",
        searchId: "cursor-epoch-lie",
        query: "malformed",
        caseSensitive: false,
        maxSessions: GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
        maxRowsPerSession: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
        maxMatches: GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
      },
      continuations: [{
        position: {
          sessionId: secondId,
          workerFp: GLOBAL_TEST_WORKER_A1,
          gridEpoch: "expected-epoch",
          beforeRow: 100,
        },
        searched: false,
      }],
      eligibleSessions: 1,
      searchedSessionIds: [],
    });
    const epochPromise = handlers.sessionsSearchGlobal(
      create(SessionsSearchGlobalRequestSchema, {
        query: "malformed",
        searchId: "cursor-epoch-lie",
        cursor,
      }),
      fixture.context(),
    );
    const commands = await worker.waitForKind(
      "search-scrollback-batch",
      malformedResults.length + 1,
    );
    const epochCommand = commands.at(-1)!;
    worker.respond(epochCommand, { entries: [{
      status: "ok",
      session_id: secondId,
      result: workerResult("wrong-epoch", {
        scanned_start_row: 90,
        scanned_end_row: 100,
        history_floor: "evicted",
      }),
    }] });
    const epochResponse = await epochPromise;
    expect(epochResponse.partials![0]?.reason)
      .toBe(GlobalSearchPartialReason.MALFORMED_RESULT);
    expect(_pendingRpcStats().pending).toBe(0);
  });
});

describe("global search cancellation and worker lanes", () => {
  test("retires before cancel reauthorization, groups once per worker, and clears pending RPCs", async () => {
    const workerA1 = fixture.installWorker(GLOBAL_TEST_WORKER_A1);
    const workerA2 = fixture.installWorker(GLOBAL_TEST_WORKER_A2);
    const firstId = sessionId(420);
    const secondId = sessionId(421);
    await fixture.insertSession({ id: firstId, workerFp: GLOBAL_TEST_WORKER_A1 });
    await fixture.insertSession({ id: secondId, workerFp: GLOBAL_TEST_WORKER_A2 });
    const owner = new GlobalSearchCursorOwner();
    const handlers = fixture.handlers(owner, new GlobalSearchWorkerLaneOwner());
    const pendingBefore = _pendingRpcStats().pending;
    const searchPromise = handlers.sessionsSearchGlobal(
      create(SessionsSearchGlobalRequestSchema, {
        query: "cancel",
        searchId: "cancel-active",
      }),
      fixture.context(),
    );
    await Promise.all([
      workerA1.waitForKind("search-scrollback-batch"),
      workerA2.waitForKind("search-scrollback-batch"),
    ]);
    expect(_pendingRpcStats().pending).toBe(pendingBefore + 2);

    await handlers.sessionsCancelGlobalSearch(
      create(SessionsCancelGlobalSearchRequestSchema, { searchId: "cancel-active" }),
      fixture.context(),
    );
    const [[cancelA1], [cancelA2]] = await Promise.all([
      workerA1.waitForKind("cancel-scrollback-search-batch"),
      workerA2.waitForKind("cancel-scrollback-search-batch"),
    ]);
    expect(cancelA1!.control.session_ids).toEqual([firstId]);
    expect(cancelA2!.control.session_ids).toEqual([secondId]);
    expect(cancelA1!.browserCommand.viewerId).toBe("global-browser:global-tab");
    expect((await connectError(searchPromise)).code).toBe(Code.Canceled);
    expect(_pendingRpcStats().pending).toBe(pendingBefore);

    const searchCountBefore = workerA1.commands.filter((command) =>
      command.control.kind === "search-scrollback-batch"
    ).length;
    await handlers.sessionsCancelGlobalSearch(
      create(SessionsCancelGlobalSearchRequestSchema, { searchId: "cancel-before" }),
      fixture.context(),
    );
    const cancelledBefore = handlers.sessionsSearchGlobal(
      create(SessionsSearchGlobalRequestSchema, {
        query: "cancel",
        searchId: "cancel-before",
      }),
      fixture.context(),
    );
    expect((await connectError(cancelledBefore)).code).toBe(Code.Canceled);
    expect(workerA1.commands.filter((command) =>
      command.control.kind === "search-scrollback-batch"
    )).toHaveLength(searchCountBefore);
  });

  test("serializes same-worker pages, leaves other workers parallel, and charges queue time to the page", async () => {
    const workerA1 = fixture.installWorker(GLOBAL_TEST_WORKER_A1);
    const workerA2 = fixture.installWorker(GLOBAL_TEST_WORKER_A2);
    const firstId = sessionId(430);
    const otherId = sessionId(431);
    await fixture.insertSession({ id: firstId, workerFp: GLOBAL_TEST_WORKER_A1 });
    await fixture.insertSession({ id: otherId, workerFp: GLOBAL_TEST_WORKER_A2 });
    const owner = new GlobalSearchCursorOwner();
    const lanes = new GlobalSearchWorkerLaneOwner();
    const handlers = fixture.handlers(owner, lanes);

    const firstController = new AbortController();
    const firstPromise = handlers.sessionsSearchGlobal(
      create(SessionsSearchGlobalRequestSchema, {
        query: "first",
        searchId: "lane-first",
      }),
      fixture.context({ signal: firstController.signal }),
    );
    const [firstA1] = await workerA1.waitForKind("search-scrollback-batch");
    const [firstA2] = await workerA2.waitForKind("search-scrollback-batch");
    expect(firstA1).toBeDefined();
    expect(firstA2).toBeDefined();

    const queuedPromise = handlers.sessionsSearchGlobal(
      create(SessionsSearchGlobalRequestSchema, {
        query: "queued",
        searchId: "lane-queued",
      }),
      fixture.context({ deviceFingerprint: "second-browser", tabId: "second-tab" }),
    );
    const queuedResponse = await queuedPromise;
    expect(queuedResponse.partials).toHaveLength(2);
    expect(queuedResponse.partials!.every((partial) =>
      partial.reason === GlobalSearchPartialReason.DEADLINE
    )).toBe(true);
    expect(workerA1.commands.filter((command) =>
      command.control.kind === "search-scrollback-batch"
    )).toHaveLength(1);
    expect(workerA2.commands.filter((command) =>
      command.control.kind === "search-scrollback-batch"
    )).toHaveLength(1);

    firstController.abort();
    expect((await connectError(firstPromise)).code).toBe(Code.Canceled);
    expect(_pendingRpcStats().pending).toBe(0);
  });

  test("maps a synchronous send failure to unavailable and releases pending state", async () => {
    const worker = fixture.installWorker(GLOBAL_TEST_WORKER_A1);
    const id = sessionId(440);
    await fixture.insertSession({ id, workerFp: GLOBAL_TEST_WORKER_A1 });
    worker.throwOnSend(true);
    const pendingBefore = _pendingRpcStats().pending;
    const response = await fixture.handlers().sessionsSearchGlobal(
      create(SessionsSearchGlobalRequestSchema, {
        query: "send-failure",
        searchId: "send-failure",
      }),
      fixture.context(),
    );
    expect(response.partials).toEqual([
      expect.objectContaining({
        sessionId: id,
        reason: GlobalSearchPartialReason.WORKER_UNAVAILABLE,
      }),
    ]);
    expect(_pendingRpcStats().pending).toBe(pendingBefore);
  });
});
