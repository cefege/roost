// Pins browser-only UI projection at the authenticated Sync feed capability seam.
// A read-only socket keeps its dashboard UI-bus subscription for legacy delivered
// counts, but receives neither retained/live reports nor legacy/apply commands.
// The focused WebSocket fixture drives the production feed, scheduler, and cleanup.

import { afterAll, beforeAll, beforeEach, expect, test } from "bun:test";
import type { ServerWebSocket } from "bun";
import { create } from "@bufbuild/protobuf";
import {
  UiCommandSchema,
  UiReportStateRequestSchema,
  UiSelectTabSchema,
} from "@roost/shared/proto/sync_pb";
import { uiBus } from "../src/buses.ts";
import { makeSyncWsHandler, type SyncWsData } from "../src/connect/sync-ws-handler.ts";
import { createSyncV2SocketState } from "../src/connect/sync-ws-v2-state.ts";
import {
  createSyncWsKeepaliveCoordFixture,
  SYNC_WS_KEEPALIVE_DASHBOARD_ID,
  type SyncWsKeepaliveCoordFixture,
} from "./sync-ws-keepalive-coord-fixture.ts";
import { PressureSocket } from "./sync-ws-keepalive-pressure-fixture.ts";

let fixture: SyncWsKeepaliveCoordFixture;

beforeAll(async () => {
  fixture = await createSyncWsKeepaliveCoordFixture();
});

beforeEach(() => {
  fixture.deps.uiStates._statesByTab.clear();
});

afterAll(async () => {
  await fixture?.close();
});

test("read-only Sync counts as subscribed but receives no seeded or live UI frames", async () => {
  const dashboardId = SYNC_WS_KEEPALIVE_DASHBOARD_ID;
  const retainedState = create(UiReportStateRequestSchema, {
    tabId: "retained-tab",
    activePath: "/retained",
    folderKey: "folder",
  });
  fixture.deps.uiStates.report({
    dashboardId,
    fingerprint: fixture.fingerprint,
    tabId: retainedState.tabId,
    state: retainedState,
  });

  const legacy = new PressureSocket(fixture, 1, 0, false);
  legacy.data.readOnly = true;
  legacy.data.tabId = null;
  legacy.data.viewerKey = null;
  const v2 = new PressureSocket(fixture, 1, 0, true);
  v2.data.flowControl = true;
  v2.data.v2 = createSyncV2SocketState();
  v2.data.readOnly = true;
  v2.data.tabId = null;
  v2.data.viewerKey = null;
  const sockets = [legacy, v2];
  const webSockets = sockets.map((socket) =>
    socket as unknown as ServerWebSocket<SyncWsData>
  );
  const beforeSubscribers = uiBus.subscriberCountFor(dashboardId);
  const handler = makeSyncWsHandler(fixture.deps, { keepaliveMs: 60_000 });
  for (const ws of webSockets) handler.open(ws);
  for (const socket of sockets) {
    clearInterval(socket.data.keepaliveTimer ?? undefined);
    socket.data.keepaliveTimer = null;
  }
  await Promise.resolve();

  expect(uiBus.subscriberCountFor(dashboardId)).toBe(beforeSubscribers + 2);
  expect(sockets.every((socket) =>
    socket.frames.every((frame) => frame.frame.case !== "uiState")
  )).toBe(true);
  const liveState = create(UiReportStateRequestSchema, {
    tabId: "live-tab",
    activePath: "/live",
    folderKey: "folder",
  });
  uiBus.publish({
    kind: "state",
    fp: fixture.fingerprint,
    tabId: liveState.tabId,
    state: liveState,
    _dashboard_id: dashboardId,
  });
  const command = create(UiCommandSchema, {
    command: {
      case: "selectTab",
      value: create(UiSelectTabSchema, { sessionId: "session" }),
    },
  });
  uiBus.publish({
    kind: "command",
    targetTabId: "",
    command,
    _dashboard_id: dashboardId,
  });
  uiBus.publish({
    kind: "apply",
    targetTabId: "retained-tab",
    targetSocketId: v2.data.v2!.socketId,
    correlationId: "correlation",
    command,
    _dashboard_id: dashboardId,
  });
  expect(sockets.every((socket) =>
    socket.frames.every((frame) =>
      frame.frame.case !== "uiState" && frame.frame.case !== "uiCommand"
    )
  )).toBe(true);

  for (const ws of webSockets) handler.close(ws);
  expect(uiBus.subscriberCountFor(dashboardId)).toBe(beforeSubscribers);
});
