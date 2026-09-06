// Owns setup and request helpers for coordinator scrollback transport tests.
// Test suites use the real worker WebSocket fixture and pending-RPC dispatch.
// It centralizes session insertion and RPC envelope construction.

import { create } from "@bufbuild/protobuf";
import { ConnectError } from "@connectrpc/connect";
import {
  SessionsSearchScrollbackRequestSchema,
  type SessionsSearchScrollbackResponse,
} from "@roost/shared/proto/coordinator_pb";
import {
  CoordWorkerUpSchema,
  type DBrowserCommand,
  WRpcErrorSchema,
  WRpcOkSchema,
} from "@roost/shared/proto/worker_transport_pb";
import type { WorkerSearchScrollbackResult } from "@roost/shared/terminal-search";
import { expect } from "bun:test";
import { makeSessionScrollbackHandlers } from "../src/connect/handlers-sessions-scrollback.ts";
import {
  helloFrame,
  startWorkerWsTransportFixture,
  type TestWorkerConnection,
  type WorkerWsTransportFixture,
} from "./worker-ws-transport-fixture.ts";

export type SearchOverrides = Partial<{
  query: string;
  searchId: string;
  caseSensitive: boolean;
  regex: boolean;
  gridEpoch: string;
  beforeRow: bigint | undefined;
  maxRows: number;
  maxMatches: number;
}>;

export interface ScrollbackSearchInvocation {
  browserCommand: DBrowserCommand;
  controlFrame: Record<string, unknown>;
  responsePromise: Promise<SessionsSearchScrollbackResponse>;
  searchId: string;
  sessionId: string;
}

export type ScrollbackTransportHarness = WorkerWsTransportFixture & {
  beginSearch: (
    worker: TestWorkerConnection,
    overrides?: SearchOverrides,
    signal?: AbortSignal,
  ) => Promise<ScrollbackSearchInvocation>;
  connectReadyWorker: () => Promise<TestWorkerConnection>;
  insertOpenSession: () => Promise<string>;
};

export async function startScrollbackTransportHarness(): Promise<ScrollbackTransportHarness> {
  const fixture = await startWorkerWsTransportFixture();
  let sessionSequence = 770;

  async function connectReadyWorker(): Promise<TestWorkerConnection> {
    const worker = fixture.connectWorker(fixture.workerFp, fixture.workerJwt);
    await worker.opened;
    worker.sendUp(helloFrame(fixture.workerFp));
    await worker.waitFor((frame) => frame.frame.case === "helloAck");
    await fixture.readyWorker(worker, fixture.workerFp);
    return worker;
  }

  async function insertOpenSession(): Promise<string> {
    sessionSequence++;
    const sessionId = `00000000-0000-4000-8000-${String(sessionSequence).padStart(12, "0")}`;
    await fixture.connectDeps.db.insertInto("sessions").values({
      id: sessionId,
      dashboard_id: fixture.dashboardId,
      worker_fp: fixture.workerFp,
      channel: sessionSequence,
      kind: "shell",
      cwd: "/tmp",
      status: "open",
      created_at: Date.now(),
    }).execute();
    return sessionId;
  }

  async function beginSearch(
    worker: TestWorkerConnection,
    overrides: SearchOverrides = {},
    signal?: AbortSignal,
  ) {
    const sessionId = await insertOpenSession();
    const query = overrides.query ?? `needle-${sessionSequence}`;
    const searchId = overrides.searchId ?? `search-${sessionSequence}`;
    const responsePromise = Promise.resolve(
      makeSessionScrollbackHandlers(fixture.connectDeps)
        .sessionsSearchScrollback(create(SessionsSearchScrollbackRequestSchema, {
          sessionId,
          searchId,
          query,
          caseSensitive: true,
          regex: false,
          gridEpoch: "browser-grid:4",
          beforeRow: 800n,
          maxRows: 100,
          maxMatches: 20,
          ...overrides,
        }), fixture.browserAuthContext(signal)),
    ) as Promise<SessionsSearchScrollbackResponse>;
    const command = await worker.waitFor((frame) => {
      if (frame.frame.case !== "browserCommand") return false;
      const candidate = JSON.parse(frame.frame.value.frameJson) as { session_id?: string };
      return candidate.session_id === sessionId;
    });
    if (command.frame.case !== "browserCommand") {
      throw new Error("expected browser command");
    }
    return {
      searchId,
      browserCommand: command.frame.value,
      controlFrame: JSON.parse(command.frame.value.frameJson) as Record<string, unknown>,
      responsePromise,
      sessionId,
    };
  }

  return {
    ...fixture,
    beginSearch,
    connectReadyWorker,
    insertOpenSession,
  };
}

export function searchResult(
  overrides: Partial<WorkerSearchScrollbackResult> = {},
): WorkerSearchScrollbackResult {
  return {
    matches: [{ row: 745, col: 3, len: 6, preview: "prefix needle" }],
    truncated: false,
    total: 1_000,
    cols: 80,
    grid_epoch: "worker-grid:9",
    scanned_start_row: 700,
    scanned_end_row: 800,
    history_floor: "evicted",
    stop_reason: "complete",
    ...overrides,
  };
}

export function sendRpcOk(
  worker: TestWorkerConnection,
  requestId: string,
  data: unknown,
): void {
  worker.sendUp(create(CoordWorkerUpSchema, {
    frame: {
      case: "rpcOk",
      value: create(WRpcOkSchema, { requestId, dataJson: JSON.stringify(data) }),
    },
  }));
}

export function sendRpcError(
  worker: TestWorkerConnection,
  requestId: string,
  message: string,
): void {
  worker.sendUp(create(CoordWorkerUpSchema, {
    frame: {
      case: "rpcError",
      value: create(WRpcErrorSchema, { requestId, message }),
    },
  }));
}

export async function expectConnectError(result: unknown): Promise<ConnectError> {
  try {
    await result;
  } catch (error) {
    expect(error).toBeInstanceOf(ConnectError);
    return error as ConnectError;
  }
  throw new Error("expected ConnectError");
}
