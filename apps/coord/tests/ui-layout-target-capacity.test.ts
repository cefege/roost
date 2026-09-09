// Pins live layout-target cardinality and Sync admission cleanup.
// Focused owner cases distinguish the two internal bounds while requiring one generic error.
// The transport case drives the production Sync handler with deterministic fake sockets.
// A per-file coordinator fixture keeps feed subscriptions and target state isolated.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { ServerWebSocket } from "bun";
import { makeSyncWsHandler, type SyncWsData } from "../src/connect/sync-ws-handler.ts";
import {
  UiLayoutApplyCapacityError,
  UiLayoutApplyOwner,
  type UiLayoutApplyTarget,
} from "../src/connect/ui-layout-apply-owner.ts";
import { createSyncV2SocketState } from "../src/connect/sync-ws-v2-state.ts";
import {
  createSyncWsKeepaliveCoordFixture,
  type SyncWsKeepaliveCoordFixture,
} from "./sync-ws-keepalive-coord-fixture.ts";
import { PressureSocket } from "./sync-ws-keepalive-pressure-fixture.ts";

const BASE_TARGET: UiLayoutApplyTarget = {
  fingerprint: "fingerprint-a",
  tabId: "tab-a",
  socketId: "socket-a",
};
let fixture: SyncWsKeepaliveCoordFixture;

beforeAll(async () => {
  fixture = await createSyncWsKeepaliveCoordFixture();
});

afterAll(async () => {
  await fixture?.close();
});

function captureCapacityError(register: () => void): UiLayoutApplyCapacityError {
  try {
    register();
  } catch (error) {
    if (error instanceof UiLayoutApplyCapacityError) return error;
    throw error;
  }
  throw new Error("expected layout target capacity rejection");
}

function makeTargetSocket(tabId: string) {
  const socket = new PressureSocket(fixture, 1, 0, true);
  socket.data.flowControl = true;
  socket.data.v2 = createSyncV2SocketState();
  socket.data.tabId = tabId;
  socket.data.viewerKey = `${fixture.fingerprint}:${tabId}`;
  return {
    socket,
    ws: socket as unknown as ServerWebSocket<SyncWsData>,
  };
}

test("distinct targets hit the per-fingerprint and aggregate caps with one generic error", () => {
  const fingerprintOwner = new UiLayoutApplyOwner({
    maxTargetsPerFingerprint: 2,
    maxTargetsTotal: 8,
  });
  fingerprintOwner.registerTarget(BASE_TARGET);
  fingerprintOwner.registerTarget({
    ...BASE_TARGET,
    tabId: "tab-b",
    socketId: "socket-b",
  });
  const fingerprintError = captureCapacityError(() => fingerprintOwner.registerTarget({
    ...BASE_TARGET,
    tabId: "tab-c",
    socketId: "socket-c",
  }));
  expect(fingerprintOwner.stats().targets).toBe(2);

  const aggregateOwner = new UiLayoutApplyOwner({
    maxTargetsPerFingerprint: 8,
    maxTargetsTotal: 2,
  });
  aggregateOwner.registerTarget(BASE_TARGET);
  aggregateOwner.registerTarget({
    ...BASE_TARGET,
    fingerprint: "fingerprint-b",
    tabId: "tab-b",
    socketId: "socket-b",
  });
  const aggregateError = captureCapacityError(() => aggregateOwner.registerTarget({
    ...BASE_TARGET,
    fingerprint: "fingerprint-c",
    tabId: "tab-c",
    socketId: "socket-c",
  }));
  expect(aggregateOwner.stats().targets).toBe(2);
  expect(fingerprintError.message).toBe(aggregateError.message);
  expect(fingerprintError.name).toBe(aggregateError.name);
  fingerprintOwner.dispose();
  aggregateOwner.dispose();
});

test("same-tuple replacement is admitted at capacity and close frees both counts", () => {
  const owner = new UiLayoutApplyOwner({
    maxTargetsPerFingerprint: 1,
    maxTargetsTotal: 1,
  });
  const closeOld = owner.registerTarget(BASE_TARGET);
  const closeReplacement = owner.registerTarget({ ...BASE_TARGET, socketId: "replacement" });
  expect(owner.stats().targets).toBe(1);
  closeOld();
  expect(owner.stats().targets).toBe(1);
  closeReplacement();
  expect(owner.stats().targets).toBe(0);

  const closeNext = owner.registerTarget({
    ...BASE_TARGET,
    tabId: "tab-next",
    socketId: "socket-next",
  });
  expect(owner.stats().targets).toBe(1);
  closeNext();
  expect(owner.stats().targets).toBe(0);
  owner.dispose();
});

test("Sync target rejection closes once and leaves no rejected feed or target", () => {
  const uiLayoutApplies = new UiLayoutApplyOwner({
    maxTargetsPerFingerprint: 1,
    maxTargetsTotal: 8,
  });
  const handler = makeSyncWsHandler({ ...fixture.deps, uiLayoutApplies }, {
    keepaliveMs: 60_000,
  });
  const admitted = makeTargetSocket("admitted-tab");
  handler.open(admitted.ws);
  clearInterval(admitted.socket.data.keepaliveTimer ?? undefined);
  admitted.socket.data.keepaliveTimer = null;
  expect(uiLayoutApplies.stats().targets).toBe(1);

  const rejected = makeTargetSocket("rejected-tab");
  handler.open(rejected.ws);
  expect(rejected.socket.closes).toEqual([[1013, "connection rejected"]]);
  expect(rejected.socket.data.feed).toBeNull();
  expect(rejected.socket.data.keepaliveTimer).toBeNull();
  expect(rejected.socket.data.v2?.snapshotDispose).toBeNull();
  expect(rejected.socket.data.v2?.layoutTargetDispose).toBeNull();
  expect(uiLayoutApplies.stats().targets).toBe(1);
  handler.close(rejected.ws);
  expect(uiLayoutApplies.stats().targets).toBe(1);

  handler.close(admitted.ws);
  expect(uiLayoutApplies.stats().targets).toBe(0);
  const admittedAfterClose = makeTargetSocket("after-close-tab");
  handler.open(admittedAfterClose.ws);
  expect(uiLayoutApplies.stats().targets).toBe(1);
  handler.close(admittedAfterClose.ws);
  expect(uiLayoutApplies.stats().targets).toBe(0);
  uiLayoutApplies.dispose();
});
