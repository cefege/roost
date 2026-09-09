// Pins global-search cursor continuation through fake routable worker handles:
// an offline session is retained for retry, and every cursor session is
// reauthorized against session close and worker deletion before it is searched.
// Sibling worker-ws-transport-global-search.test.ts owns first-page fan-out,
// caps, and per-session partial families.

import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  GlobalSearchPartialReason,
  SessionsSearchGlobalRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
  GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
  GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
} from "@roost/shared/terminal-search";
import {
  GlobalSearchCursorOwner,
  type GlobalSearchCursorBinding,
} from "../src/connect/global-search-cursors.ts";
import { GlobalSearchWorkerLaneOwner } from "../src/connect/global-search-worker-lanes.ts";
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
  test("retains an unvisited offline session for cursor retry", async () => {
    const id = sessionId(305);
    await fixture.insertSession({ id, workerFp: GLOBAL_TEST_WORKER_A1 });
    const owner = new GlobalSearchCursorOwner();
    const lanes = new GlobalSearchWorkerLaneOwner();
    const handlers = fixture.handlers(owner, lanes);
    const request = { query: "retry", searchId: "offline-retry" };
    const first = await handlers.sessionsSearchGlobal(
      create(SessionsSearchGlobalRequestSchema, request),
      fixture.context(),
    );
    expect(first.partials![0]?.reason)
      .toBe(GlobalSearchPartialReason.WORKER_UNAVAILABLE);
    expect(first.nextCursor).toBeDefined();
    expect(first.searchedSessions).toBe(0);
    expect(first.eligibleSessions).toBe(1);

    const worker = fixture.installWorker(GLOBAL_TEST_WORKER_A1);
    const retryPromise = handlers.sessionsSearchGlobal(
      create(SessionsSearchGlobalRequestSchema, {
        ...request,
        cursor: first.nextCursor,
      }),
      fixture.context(),
    );
    const [command] = await worker.waitForKind("search-scrollback-batch");
    expect(command!.control.sessions).toEqual([{
      session_id: id,
      grid_epoch: "",
    }]);
    worker.respond(command!, { entries: [okEntry(id)] });
    const retried = await retryPromise;
    expect(retried.partials).toHaveLength(0);
    expect(retried.nextCursor).toBeUndefined();
    expect(retried.searchedSessions).toBe(1);
    expect(retried.eligibleSessions).toBe(1);
  });
  test("reauthorizes every cursor session after close or worker deletion", async () => {

    const owner = new GlobalSearchCursorOwner();
    const lanes = new GlobalSearchWorkerLaneOwner();
    const closedId = sessionId(310);
    const deletedWorkerId = sessionId(311);
    await fixture.insertSession({ id: closedId, workerFp: GLOBAL_TEST_WORKER_A1 });
    await fixture.insertSession({ id: deletedWorkerId, workerFp: GLOBAL_TEST_WORKER_A2 });
    fixture.installWorker(GLOBAL_TEST_WORKER_A1);
    fixture.installWorker(GLOBAL_TEST_WORKER_A2);
    const binding: GlobalSearchCursorBinding = {
      deviceFingerprint: "global-browser",
      tabId: "global-tab",
      searchId: "reauthorize",
      query: "needle",
      caseSensitive: false,
      maxSessions: GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
      maxRowsPerSession: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
      maxMatches: GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
    };
    const cursor = owner.issueCursor({
      binding,
      continuations: [
        {
          position: {
            sessionId: closedId,
            workerFp: GLOBAL_TEST_WORKER_A1,
            gridEpoch: "epoch-a",
            beforeRow: 100,
          },
          searched: false,
        },
        {
          position: {
            sessionId: deletedWorkerId,
            workerFp: GLOBAL_TEST_WORKER_A2,
            gridEpoch: "epoch-b",
            beforeRow: 100,
          },
          searched: false,
        },
      ],
      eligibleSessions: 2,
      searchedSessionIds: [],
    });
    await fixture.db.updateTable("sessions").set({ status: "closed" })
      .where("id", "=", closedId).execute();
    await fixture.db.updateTable("workers").set({ deleted_at_ms: Date.now() })
      .where("fp", "=", GLOBAL_TEST_WORKER_A2).execute();

    const response = await fixture.handlers(owner, lanes).sessionsSearchGlobal(
      create(SessionsSearchGlobalRequestSchema, {
        query: "needle",
        searchId: "reauthorize",
        cursor,
      }),
      fixture.context(),
    );
    expect(response.eligibleSessions).toBe(2);
    expect(response.searchedSessions).toBe(0);
    expect(response.partials).toEqual([
      { $typeName: "roost.v1.SessionsSearchGlobalPartial", sessionId: closedId, reason: GlobalSearchPartialReason.SESSION_CLOSED },
      { $typeName: "roost.v1.SessionsSearchGlobalPartial", sessionId: deletedWorkerId, reason: GlobalSearchPartialReason.SESSION_CLOSED },
    ]);
    expect(response.nextCursor).toBeUndefined();
  });
});
