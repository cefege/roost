// Exercises authorized scrollback relays across the real worker WebSocket transport.
// The suite pins exact browser-command JSON, protobuf paging metadata, and failures.
// It depends on the isolated worker transport fixture and real pending-RPC dispatch.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { Code } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  ScrollbackHistoryFloor,
  SearchStopReason,
  SessionsCancelScrollbackSearchRequestSchema,
  SessionsGetScrollbackCellsRequestSchema,
  SessionsSearchScrollbackRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  TERMINAL_SEARCH_GRID_EPOCH_MAX_LENGTH,
  TERMINAL_SEARCH_ID_MAX_LENGTH,
  TERMINAL_SEARCH_MAX_MATCHES,
  TERMINAL_SEARCH_MAX_ROWS,
  TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS,
  TERMINAL_SEARCH_RPC_DEADLINE_MS,
} from "@roost/shared/terminal-search";
import { tabIdKey } from "../src/connect/auth-interceptor.ts";
import { makeSessionScrollbackHandlers } from "../src/connect/handlers-sessions-scrollback.ts";
import { _pendingRpcStats } from "../src/router/pending-rpcs.ts";
import { connectWorkers } from "../src/connect/worker-registry.ts";
import {
  expectConnectError,
  searchResult,
  sendRpcError,
  sendRpcOk,
  startScrollbackTransportHarness,
  type ScrollbackTransportHarness,
  type SearchOverrides,
} from "./scrollback-transport-test-harness.ts";
let harness: ScrollbackTransportHarness;
let beginSearch: ScrollbackTransportHarness["beginSearch"];
let browserAuthContext: ScrollbackTransportHarness["browserAuthContext"];
let closeWorkerSockets: ScrollbackTransportHarness["closeWorkerSockets"];
let connectDeps: ScrollbackTransportHarness["connectDeps"];
let connectReadyWorker: ScrollbackTransportHarness["connectReadyWorker"];
let insertOpenSession: ScrollbackTransportHarness["insertOpenSession"];
let workerFp: string;
beforeAll(async () => {
  harness = await startScrollbackTransportHarness();
  ({
    beginSearch,
    browserAuthContext,
    closeWorkerSockets,
    connectDeps,
    connectReadyWorker,
    insertOpenSession,
    workerFp,
  } = harness);
});
afterAll(async () => { await harness?.cleanup(); });

describe("worker↔coord scrollback transport", () => {
  test("scrollback cell epoch and floor survive both transport directions", async () => {
    const worker = await connectReadyWorker();
    const sessionId = await insertOpenSession();
    const responsePromise = makeSessionScrollbackHandlers(connectDeps)
      .sessionsGetScrollbackCells(create(SessionsGetScrollbackCellsRequestSchema, {
        sessionId,
        endRow: 500n,
        maxRows: 1_000,
        gridEpoch: "browser-grid:4",
      }), browserAuthContext());
    const command = await worker.waitFor((frame) => frame.frame.case === "browserCommand");
    if (command.frame.case !== "browserCommand") throw new Error("expected browser command");
    expect(JSON.parse(command.frame.value.frameJson)).toEqual({
      kind: "get-scrollback-cells",
      request_id: command.frame.value.requestId,
      session_id: sessionId,
      grid_epoch: "browser-grid:4",
      end_row: 500,
      max_rows: 1_000,
    });
    sendRpcOk(worker, command.frame.value.requestId, {
      rows: [], cols: 80, total: 500, start_row: 0, end_row: 500,
      grid_epoch: "worker-grid:9", history_floor: "resize_replay",
    });
    const response = await responsePromise;
    expect(response.gridEpoch).toBe("worker-grid:9");
    expect(response.historyFloor).toBe(ScrollbackHistoryFloor.RESIZE_REPLAY);
    worker.close();
  });

  test("cell worker errors remain internal instead of becoming timeouts", async () => {
    const worker = await connectReadyWorker();
    const sessionId = await insertOpenSession();
    const responsePromise = makeSessionScrollbackHandlers(connectDeps)
      .sessionsGetScrollbackCells(create(SessionsGetScrollbackCellsRequestSchema, {
        sessionId, endRow: 500n, maxRows: 1_000, gridEpoch: "stale-grid",
      }), browserAuthContext());
    const command = await worker.waitFor((frame) => frame.frame.case === "browserCommand");
    if (command.frame.case !== "browserCommand") throw new Error("expected browser command");
    sendRpcError(worker, command.frame.value.requestId, "grid epoch changed");
    const error = await expectConnectError(responsePromise);
    expect(error.code).toBe(Code.Internal);
    expect(error.rawMessage).toBe("grid epoch changed");
    worker.close();
  });

  test("search forwards exact paging JSON and maps every stop and floor enum", async () => {
    const worker = await connectReadyWorker();
    const unicodeBoundaryQuery = "\u{1F642}".repeat(TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS);
    const cases = [
      ["row_limit", SearchStopReason.ROW_LIMIT, "evicted", ScrollbackHistoryFloor.EVICTED, 700n],
      ["complete", SearchStopReason.COMPLETE, "none", ScrollbackHistoryFloor.UNSPECIFIED, undefined],
      ["match_limit", SearchStopReason.MATCH_LIMIT, "resize_replay", ScrollbackHistoryFloor.RESIZE_REPLAY, undefined],
      ["deadline", SearchStopReason.DEADLINE, "none", ScrollbackHistoryFloor.UNSPECIFIED, undefined],
      ["epoch_changed", SearchStopReason.EPOCH_CHANGED, "none", ScrollbackHistoryFloor.UNSPECIFIED, undefined],
    ] as const;
    for (const [index, [stopReason, protoStop, historyFloor, protoFloor, nextRow]] of cases.entries()) {
      const query = index === 0 ? unicodeBoundaryQuery : `map-${stopReason}`;
      const scannedStartRow = stopReason === "complete" && historyFloor === "none"
        ? 0
        : 700;
      const scannedEndRow = index === 1 ? 100 : 800;
      const search = await beginSearch(worker, {
        query,
        beforeRow: index === 1 ? undefined : 800n,
      });
      if (index === 0) {
        expect(search.browserCommand.browserId).toBe("browser-fp:test-tab");
        expect(search.browserCommand.viewerId).toBe("browser-fp:test-tab");
        expect(search.controlFrame).toEqual({
          kind: "search-scrollback",
          request_id: search.browserCommand.requestId,
          session_id: search.sessionId,
          search_id: search.searchId,
          grid_epoch: "browser-grid:4",
          query,
          case_sensitive: true,
          regex: false,
          before_row: 800,
          max_rows: 100,
          max_matches: 20,
        });
      } else if (index === 1) {
        expect(search.controlFrame).not.toHaveProperty("before_row");
      }
      sendRpcOk(worker, search.browserCommand.requestId, searchResult({
        history_floor: historyFloor,
        stop_reason: stopReason,
        scanned_start_row: scannedStartRow,
        scanned_end_row: scannedEndRow,
        grid_epoch: stopReason === "epoch_changed" ? "worker-grid:9" : "browser-grid:4",
        ...(stopReason === "match_limit"
          ? { matches: Array.from({ length: 20 }, () => ({
              row: 745, col: 3, len: 6, preview: "needle",
            })) }
          : {}),
        ...(index === 1 ? { matches: [{ row: 45, col: 3, len: 6, preview: "needle" }] } : {}),
        ...(nextRow === undefined
          ? {}
          : { next_before_row: Number(nextRow) }),
        ...(stopReason === "epoch_changed" ? { matches: [] } : {}),
        truncated: stopReason === "match_limit" || stopReason === "deadline",
      }));
      const response = await search.responsePromise;
      expect(response.stopReason).toBe(protoStop);
      expect(response.historyFloor).toBe(protoFloor);
      expect(response.nextBeforeRow).toBe(nextRow);
      expect(response.scannedStartRow).toBe(BigInt(scannedStartRow));
      expect(response.scannedEndRow).toBe(BigInt(scannedEndRow));
      expect(response.scrollbackTotal).toBe(1_000n);
      expect(response.gridEpoch).toBe(
        stopReason === "epoch_changed" ? "worker-grid:9" : "browser-grid:4",
      );
      if (index === 0) {
        expect(response.cols).toBe(80);
        expect(response.truncated).toBe(false);
        expect(response.matches?.[0]).toMatchObject({
          row: 745n, col: 3, len: 6, preview: "prefix needle",
        });
      }
    }
    worker.close();
  });

  test("search distinguishes invalid regex RPC errors from ordinary worker errors", async () => {
    const worker = await connectReadyWorker();
    for (const [message, code] of [

      ["invalid regex: unterminated group", Code.InvalidArgument],
      ["grid epoch changed", Code.Internal],
    ] as const) {
      const search = await beginSearch(worker, { query: message, regex: true });
      sendRpcError(worker, search.browserCommand.requestId, message);
      const error = await expectConnectError(search.responsePromise);
      expect(error.code).toBe(code);
      expect(error.rawMessage).toBe(message);
    }
    worker.close();
  });
  test("explicit cancel forwards the same tab-owned search identity", async () => {
    const worker = await connectReadyWorker();
    const pendingBefore = _pendingRpcStats().pending;
    const search = await beginSearch(worker);
    await makeSessionScrollbackHandlers(connectDeps).sessionsCancelScrollbackSearch(
      create(SessionsCancelScrollbackSearchRequestSchema, {
        sessionId: search.sessionId,
        searchId: search.searchId,
      }),
      browserAuthContext(),
    );
    const cancellation = await worker.waitFor((frame) => {
      if (frame.frame.case !== "browserCommand") return false;
      const control = JSON.parse(frame.frame.value.frameJson) as { kind?: string };
      return control.kind === "cancel-scrollback-search";
    });
    if (cancellation.frame.case !== "browserCommand") throw new Error("expected cancellation");
    expect(cancellation.frame.value.browserId).toBe("browser-fp:test-tab");
    expect(JSON.parse(cancellation.frame.value.frameJson)).toEqual({
      kind: "cancel-scrollback-search",
      request_id: search.searchId,
      session_id: search.sessionId,
      search_request_id: search.searchId,
    });
    sendRpcError(worker, search.browserCommand.requestId, "scrollback search superseded");
    const error = await expectConnectError(search.responsePromise);
    expect(error.code).toBe(Code.Internal);
    expect(_pendingRpcStats().pending).toBe(pendingBefore);
    worker.close();
  });

  test("invalid search bounds fail before command send or pending allocation", async () => {
    const worker = await connectReadyWorker();
    const sessionId = await insertOpenSession();
    const pendingBefore = _pendingRpcStats().pending;
    const invalidRequests: SearchOverrides[] = [
      { searchId: "" },
      { searchId: "s".repeat(TERMINAL_SEARCH_ID_MAX_LENGTH + 1) },
      { gridEpoch: "e".repeat(TERMINAL_SEARCH_GRID_EPOCH_MAX_LENGTH + 1) },
      { query: "\u{1F642}".repeat(TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS + 1) },
      { maxRows: 0 },
      { maxRows: TERMINAL_SEARCH_MAX_ROWS + 1 },
      { maxMatches: 0 },
      { maxMatches: TERMINAL_SEARCH_MAX_MATCHES + 1 },
      { beforeRow: -1n },
      { beforeRow: BigInt(Number.MAX_SAFE_INTEGER) + 1n },
    ];
    for (const override of invalidRequests) {
      const error = await expectConnectError(
        makeSessionScrollbackHandlers(connectDeps).sessionsSearchScrollback(
          create(SessionsSearchScrollbackRequestSchema, {
            sessionId, searchId: "valid-search", query: "needle",
            gridEpoch: "grid", beforeRow: 100n,
            maxRows: 100, maxMatches: 20, ...override,
          }),
          browserAuthContext(),
        ),
      );
      expect(error.code).toBe(Code.InvalidArgument);
    }
    expect(_pendingRpcStats().pending).toBe(pendingBefore);
    await expect(worker.waitFor(
      (frame) => frame.frame.case === "browserCommand",
      50,
    )).rejects.toThrow("waitFor timeout");
    worker.close();
  });

  test("a search with no tab id is rejected before any command reaches the worker", async () => {
    const worker = await connectReadyWorker();
    const sessionId = await insertOpenSession();
    const pendingBefore = _pendingRpcStats().pending;
    const authorized = browserAuthContext();
    const withoutTabId = {
      signal: authorized.signal,
      values: {
        get: (key: unknown) =>
          key === tabIdKey ? undefined : authorized.values.get(key as never),
      },
    } as unknown as typeof authorized;
    const error = await expectConnectError(
      makeSessionScrollbackHandlers(connectDeps).sessionsSearchScrollback(
        create(SessionsSearchScrollbackRequestSchema, {
          sessionId, searchId: "no-tab", query: "needle",
          gridEpoch: "grid", maxRows: 100, maxMatches: 20,
        }),
        withoutTabId,
      ),
    );
    expect(error.code).toBe(Code.InvalidArgument);
    expect(error.rawMessage).toContain("x-roost-tab-id");
    expect(_pendingRpcStats().pending).toBe(pendingBefore);
    await expect(worker.waitFor(
      (frame) => frame.frame.case === "browserCommand",
      50,
    )).rejects.toThrow("waitFor timeout");
    worker.close();
  });

  test("malformed and unspecified worker results fail internal", async () => {
    const worker = await connectReadyWorker();
    const malformedResults: unknown[] = [
      { ...searchResult(), stop_reason: "unspecified" },
      { ...searchResult(), history_floor: "unknown" },
      { ...searchResult({ stop_reason: "row_limit" }), next_before_row: 699 },
      {
        ...searchResult({ grid_epoch: "browser-grid:4", stop_reason: "row_limit" }),
        scanned_start_row: 799,
        next_before_row: 799,
      },
      {
        ...searchResult({ grid_epoch: "browser-grid:4" }),
        matches: Array.from({ length: 21 }, () => searchResult().matches[0]!),
      },
      { ...searchResult(), scanned_end_row: Number.MAX_SAFE_INTEGER + 1 },
      {
        ...searchResult(),
        scanned_start_row: 700,
        matches: [{ row: 699, col: 0, len: 1, preview: "outside" }],
      },
      {
        ...searchResult(),
        matches: [{
          row: 745, col: 0, len: 1,
          preview: "\u{1F642}".repeat(513),
        }],
      },
      { ...searchResult(), cols: 0x1_0000_0000 },
      {
        ...searchResult({
          grid_epoch: "browser-grid:4",
          stop_reason: "match_limit",
          truncated: true,
        }),
        matches: [searchResult().matches[0]!],
      },
      { ...searchResult({ grid_epoch: "wrong-grid:1" }), history_floor: "evicted" },
    ];
    for (const [index, result] of malformedResults.entries()) {
      const search = await beginSearch(worker, { query: `malformed-${index}` });
      sendRpcOk(worker, search.browserCommand.requestId, result);
      const error = await expectConnectError(search.responsePromise);
      expect(error.code).toBe(Code.Internal);
      expect(error.rawMessage).toBe("malformed scrollback search result");
    }
    worker.close();
  });

  test("a synchronous transport send failure clears its pending search", async () => {
    const worker = await connectReadyWorker();
    const sessionId = await insertOpenSession();
    const workerHandle = connectWorkers.get(workerFp);
    if (!workerHandle) throw new Error("expected connected worker");
    const transportSend = workerHandle.send;
    const pendingBefore = _pendingRpcStats().pending;
    workerHandle.send = () => { throw new Error("injected send failure"); };
    try {
      const error = await expectConnectError(
        makeSessionScrollbackHandlers(connectDeps).sessionsSearchScrollback(
          create(SessionsSearchScrollbackRequestSchema, {
            sessionId, searchId: "valid-search", query: "needle",
            gridEpoch: "grid", beforeRow: 100n,
            maxRows: 100, maxMatches: 20,
          }),
          browserAuthContext(),
        ),
      );
      expect(error.code).toBe(Code.Unavailable);
      expect(_pendingRpcStats().pending).toBe(pendingBefore);
    } finally {
      workerHandle.send = transportSend;
      worker.close();
    }
  });

  test("worker disconnect rejects an in-flight search as unavailable", async () => {
    const worker = await connectReadyWorker();
    const search = await beginSearch(worker);
    closeWorkerSockets(workerFp);
    const error = await expectConnectError(search.responsePromise);
    expect(error.code).toBe(Code.Unavailable);
  });

  test("worker silence maps the outer deadline to search unavailable", async () => {
    const worker = await connectReadyWorker();
    const search = await beginSearch(worker);
    const error = await expectConnectError(search.responsePromise);
    expect(error.code).toBe(Code.Unavailable);
    expect(error.rawMessage).toBe("scrollback search timed out");
    worker.close();
  }, TERMINAL_SEARCH_RPC_DEADLINE_MS + 3_000);
});
