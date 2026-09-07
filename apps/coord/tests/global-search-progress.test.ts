// Pins the two progress rules of dashboard-wide search that the fan-out and
// cursor owner enforce together: a page that rescans nothing at the row it was
// given must not hand that row back, and a search must carry a tab id so
// supersession can only ever mean "this tab replaced its own search".
// Uses the shared global-search fixture; no network listeners.

import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
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
  GLOBAL_TEST_DASHBOARD_A,
  GLOBAL_TEST_WORKER_A1,
  startGlobalSearchTestFixture,
  type GlobalSearchTestFixture,
} from "./global-search-test-fixture.ts";

function sessionId(sequence: number): string {
  return `00000000-0000-4000-8000-${String(sequence).padStart(12, "0")}`;
}

function binding(searchId: string): GlobalSearchCursorBinding {
  return {
    dashboardId: GLOBAL_TEST_DASHBOARD_A,
    deviceFingerprint: "global-browser",
    tabId: "global-tab",
    searchId,
    query: "needle",
    caseSensitive: false,
    maxSessions: GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
    maxRowsPerSession: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
    maxMatches: GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
  };
}

let fixture: GlobalSearchTestFixture;
beforeAll(async () => { fixture = await startGlobalSearchTestFixture(); });
beforeEach(async () => { await fixture.reset(); });
afterAll(async () => { await fixture.cleanup(); });

describe("global search page progress", () => {
  test("ends a session that rescans nothing at the row it was given", async () => {
    const id = sessionId(420);
    await fixture.insertSession({ id, workerFp: GLOBAL_TEST_WORKER_A1 });
    const worker = fixture.installWorker(GLOBAL_TEST_WORKER_A1);
    const owner = new GlobalSearchCursorOwner();
    const cursor = owner.issueCursor({
      binding: binding("stalled-deadline"),
      continuations: [{
        position: {
          sessionId: id,
          workerFp: GLOBAL_TEST_WORKER_A1,
          gridEpoch: "epoch-stall",
          beforeRow: 512,
        },
        searched: false,
      }],
      eligibleSessions: 1,
      searchedSessionIds: [],
    });
    const responsePromise = fixture
      .handlers(owner, new GlobalSearchWorkerLaneOwner())
      .sessionsSearchGlobal(
        create(SessionsSearchGlobalRequestSchema, {
          query: "needle",
          searchId: "stalled-deadline",
          cursor,
        }),
        fixture.context(),
      );
    const [command] = await worker.waitForKind("search-scrollback-batch");
    expect(command!.control.sessions).toEqual([{
      session_id: id,
      grid_epoch: "epoch-stall",
      before_row: 512,
    }]);
    worker.respond(command!, { entries: [{
      status: "ok",
      session_id: id,
      result: {
        matches: [],
        truncated: true,
        scrollback_total: 4_000,
        cols: 80,
        grid_epoch: "epoch-stall",
        scanned_start_row: 512,
        scanned_end_row: 512,
        history_floor: "none",
        stop_reason: "deadline",
      },
    }] });
    const response = await responsePromise;
    expect(response.partials).toEqual([{
      $typeName: "roost.v1.SessionsSearchGlobalPartial",
      sessionId: id,
      reason: GlobalSearchPartialReason.DEADLINE,
    }]);
    // No cursor: another page would repeat this exact request forever, burning
    // a page deadline and a worker lane without reading a row.
    expect(response.nextCursor).toBeUndefined();
    expect(response.searchedSessions).toBe(1);
  });

  test("keeps paging a session whose page advanced past the requested row", async () => {
    const id = sessionId(421);
    await fixture.insertSession({ id, workerFp: GLOBAL_TEST_WORKER_A1 });
    const worker = fixture.installWorker(GLOBAL_TEST_WORKER_A1);
    const owner = new GlobalSearchCursorOwner();
    const cursor = owner.issueCursor({
      binding: binding("advancing-deadline"),
      continuations: [{
        position: {
          sessionId: id,
          workerFp: GLOBAL_TEST_WORKER_A1,
          gridEpoch: "epoch-advance",
          beforeRow: 512,
        },
        searched: false,
      }],
      eligibleSessions: 1,
      searchedSessionIds: [],
    });
    const responsePromise = fixture
      .handlers(owner, new GlobalSearchWorkerLaneOwner())
      .sessionsSearchGlobal(
        create(SessionsSearchGlobalRequestSchema, {
          query: "needle",
          searchId: "advancing-deadline",
          cursor,
        }),
        fixture.context(),
      );
    const [command] = await worker.waitForKind("search-scrollback-batch");
    worker.respond(command!, { entries: [{
      status: "ok",
      session_id: id,
      result: {
        matches: [{ row: 300, col: 1, len: 6, preview: "needle" }],
        truncated: true,
        scrollback_total: 4_000,
        cols: 80,
        grid_epoch: "epoch-advance",
        scanned_start_row: 200,
        scanned_end_row: 512,
        history_floor: "none",
        stop_reason: "deadline",
      },
    }] });
    const response = await responsePromise;
    expect(response.nextCursor).toBeDefined();
    expect(response.matches).toHaveLength(1);
  });

  test("rejects a search whose request carries no tab id", async () => {
    await fixture.insertSession({ id: sessionId(430), workerFp: GLOBAL_TEST_WORKER_A1 });
    fixture.installWorker(GLOBAL_TEST_WORKER_A1);
    const rejection = await Promise.resolve(fixture.handlers().sessionsSearchGlobal(
      create(SessionsSearchGlobalRequestSchema, { query: "needle", searchId: "no-tab" }),
      fixture.context({ omitTabId: true }),
    )).then(() => null, (error: unknown) => error);
    expect(rejection).toBeInstanceOf(ConnectError);
    expect((rejection as ConnectError).code).toBe(Code.InvalidArgument);
    expect((rejection as ConnectError).rawMessage).toContain("x-roost-tab-id");
  });
});
