// Separates scrollback forwarding so its bidirectional epoch contract stays focused.
// Bun's test runner invokes this suite with an isolated worker transport fixture.
// It depends on the real session scrollback handlers and protobuf framing.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  WRpcErrorSchema,
  WRpcOkSchema,
} from "@roost/shared/proto/worker_transport_pb";
import {
  ScrollbackHistoryFloor,
  SessionsGetScrollbackCellsRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import { makeSessionScrollbackHandlers } from "../src/connect/handlers-sessions-scrollback.ts";
import {
  helloFrame,
  startWorkerWsTransportFixture,
  type WorkerWsTransportFixture,
} from "./worker-ws-transport-fixture.ts";

let fixture: WorkerWsTransportFixture;
let browserAuthContext: WorkerWsTransportFixture["browserAuthContext"];
let connectDeps: WorkerWsTransportFixture["connectDeps"];
let connectWorker: WorkerWsTransportFixture["connectWorker"];
let dashboardId: string;
let readyWorker: WorkerWsTransportFixture["readyWorker"];
let workerFp: string;
let workerJwt: string;

beforeAll(async () => {
  fixture = await startWorkerWsTransportFixture();
  ({
    browserAuthContext,
    connectDeps,
    connectWorker,
    dashboardId,
    readyWorker,
    workerFp,
    workerJwt,
  } = fixture);
});

afterAll(async () => { await fixture?.cleanup(); });

describe("worker↔coord raw-WS transport", () => {
  test("scrollback epoch tokens survive browser to worker and worker to browser", async () => {
    const w = connectWorker(workerFp, workerJwt);
    await w.opened;
    w.sendUp(helloFrame(workerFp));
    await w.waitFor((frame) => frame.frame.case === "helloAck");
    await readyWorker(w, workerFp);

    const sessionId = "00000000-0000-4000-8000-000000000777";
    await connectDeps.db.insertInto("sessions").values({
      id: sessionId,
      dashboard_id: dashboardId,
      worker_fp: workerFp,
      channel: 7,
      kind: "shell",
      cwd: "/tmp",
      status: "open",
      created_at: Date.now(),
    }).onConflict((conflict) => conflict.column("id").doNothing()).execute();
    const handlers = makeSessionScrollbackHandlers(connectDeps);
    const authCtx = browserAuthContext();
    const responsePromise = handlers.sessionsGetScrollbackCells(
      create(SessionsGetScrollbackCellsRequestSchema, {
        sessionId,
        endRow: 500n,
        maxRows: 1000,
        gridEpoch: "browser-grid:4",
      }),
      authCtx,
    );

    const command = await w.waitFor((frame) => frame.frame.case === "browserCommand");
    expect(command.frame.case).toBe("browserCommand");
    const browserCommand = command.frame.value as { requestId: string; frameJson: string };
    const controlFrame = JSON.parse(browserCommand.frameJson) as unknown;
    expect(controlFrame).toMatchObject({
      kind: "get-scrollback-cells",
      session_id: sessionId,
      grid_epoch: "browser-grid:4",
      end_row: 500,
      max_rows: 1000,
    });

    w.sendUp(create(CoordWorkerUpSchema, {
      frame: { case: "rpcOk", value: create(WRpcOkSchema, {
        requestId: browserCommand.requestId,
        dataJson: JSON.stringify({
          rows: [],
          cols: 80,
          total: 500,
          start_row: 0,
          end_row: 500,
          grid_epoch: "worker-grid:9",
          history_floor: "resize_replay",
        }),
      }) },
    }));

    const response = await responsePromise;
    expect(response.gridEpoch).toBe("worker-grid:9");
    // The floor REASON has to survive the coord hop too, or the browser is told
    // history is missing with no way to say whether it is recoverable.
    expect(response.historyFloor).toBe(ScrollbackHistoryFloor.RESIZE_REPLAY);
    w.close();
  });

  test("scrollback worker errors are preserved instead of mislabeled as timeouts", async () => {
    const w = connectWorker(workerFp, workerJwt);
    await w.opened;
    w.sendUp(helloFrame(workerFp));
    await w.waitFor((frame) => frame.frame.case === "helloAck");
    await readyWorker(w, workerFp);

    const sessionId = "00000000-0000-4000-8000-000000000778";
    await connectDeps.db.insertInto("sessions").values({
      id: sessionId,
      dashboard_id: dashboardId,
      worker_fp: workerFp,
      channel: 8,
      kind: "shell",
      cwd: "/tmp",
      status: "open",
      created_at: Date.now(),
    }).onConflict((conflict) => conflict.column("id").doNothing()).execute();
    const handlers = makeSessionScrollbackHandlers(connectDeps);
    const authCtx = browserAuthContext();
    const responsePromise = handlers.sessionsGetScrollbackCells(
      create(SessionsGetScrollbackCellsRequestSchema, {
        sessionId,
        endRow: 500n,
        maxRows: 1000,
        gridEpoch: "stale-grid",
      }),
      authCtx,
    );

    const command = await w.waitFor((frame) => frame.frame.case === "browserCommand");
    const browserCommand = command.frame.value as { requestId: string };
    w.sendUp(create(CoordWorkerUpSchema, {
      frame: { case: "rpcError", value: create(WRpcErrorSchema, {
        requestId: browserCommand.requestId,
        message: "grid epoch changed",
      }) },
    }));

    let caught: unknown;
    try {
      await responsePromise;
    } catch (error) {
      caught = error;
    }
    expect(caught).toBeInstanceOf(ConnectError);
    expect((caught as ConnectError).code).toBe(Code.Internal);
    expect((caught as ConnectError).rawMessage).toBe("grid epoch changed");
    w.close();
  });
});
