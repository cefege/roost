// Pins acknowledged layout apply at the live Sync-v2 transport boundary.
// Bun drives the production handler, feed, scheduler, ingress parser, and explicit apply owner.
// Fake sockets prove eligibility, exact generation filtering, pre-readiness results, and cleanup.
// The per-file coordinator fixture prevents target or pending state from crossing test processes.

import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import type { ServerWebSocket } from "bun";
import { create, toBinary } from "@bufbuild/protobuf";
import { layoutDocumentToProto } from "@roost/shared/layout-document-proto";
import {
  SyncClientFrameSchema,
  SyncDomain,
  UiApplyLayoutOutcome,
  UiApplyLayoutResultSchema,
  UiApplyLayoutSchema,
  UiCommandSchema,
  UiReportStateRequestSchema,
} from "@roost/shared/proto/sync_pb";
import { uiBus } from "../src/buses.ts";
import { makeSyncWsHandler, type SyncWsData } from "../src/connect/sync-ws-handler.ts";
import { createSyncV2SocketState } from "../src/connect/sync-ws-v2-state.ts";
import type { UiLayoutApplyPublication } from "../src/connect/ui-layout-apply-owner.ts";
import {
  createSyncWsKeepaliveCoordFixture,
  SYNC_WS_KEEPALIVE_DASHBOARD_ID,
  type SyncWsKeepaliveCoordFixture,
} from "./sync-ws-keepalive-coord-fixture.ts";
import {
  PressureSocket,
  type PressureHandler,
} from "./sync-ws-keepalive-pressure-fixture.ts";

let fixture: SyncWsKeepaliveCoordFixture;
const dashboardId = SYNC_WS_KEEPALIVE_DASHBOARD_ID;

beforeAll(async () => {
  fixture = await createSyncWsKeepaliveCoordFixture();
});

beforeEach(() => {
  fixture.deps.uiLayoutApplies.dispose();
  fixture.deps.uiStates._statesByTab.clear();
});

afterAll(async () => {
  fixture?.deps.uiStates._statesByTab.clear();
  await fixture?.close();
});

function layoutDocument() {
  return layoutDocumentToProto({
    schema_version: 1,
    root: {
      kind: "leaf",
      leaf_key: "leaf-a",
      slot_keys: [],
      selected_slot_key: null,
    },
    focused_leaf_key: "leaf-a",
    bindings: [],
  });
}

function layoutCommand() {
  const document = layoutDocument();
  return create(UiCommandSchema, {
    command: {
      case: "applyLayout",
      value: create(UiApplyLayoutSchema, { document }),
    },
  });
}

function makeV2Socket(options: {
  tabId?: string;
  readOnly?: boolean;
  sendResult?: number;
} = {}) {
  const socket = new PressureSocket(fixture, 1, 0, true);
  const tabId = options.tabId;
  socket.data.flowControl = true;
  socket.data.v2 = createSyncV2SocketState();
  socket.data.readOnly = options.readOnly ?? false;
  socket.data.tabId = tabId ?? null;
  socket.data.viewerKey = !socket.data.readOnly && tabId
    ? `${fixture.fingerprint}:${tabId}`
    : null;
  socket.dataSendResult = options.sendResult ?? 1;
  return {
    socket,
    ws: socket as unknown as ServerWebSocket<SyncWsData>,
  };
}

function stopKeepalive(socket: PressureSocket): void {
  clearInterval(socket.data.keepaliveTimer ?? undefined);
  socket.data.keepaliveTimer = null;
}

function publishApply(publication: UiLayoutApplyPublication): void {
  uiBus.publish({
    kind: "apply",
    targetTabId: publication.tabId,
    targetSocketId: publication.socketId,
    correlationId: publication.correlationId,
    command: layoutCommand(),
    _dashboard_id: publication.dashboardId,
  });
}

function sendResult(
  handler: PressureHandler,
  ws: ServerWebSocket<SyncWsData>,
  socketId: string,
  correlationId: string,
  outcome = UiApplyLayoutOutcome.APPLIED,
): void {
  handler.message(ws, toBinary(SyncClientFrameSchema, create(SyncClientFrameSchema, {
    socketId,
    command: {
      case: "uiApplyLayoutResult",
      value: create(UiApplyLayoutResultSchema, { correlationId, outcome }),
    },
  })) as unknown as Buffer);
}

describe("Sync-v2 layout target eligibility", () => {
  test("registers only a writable tab-bound v2 socket after subscribed send succeeds", () => {
    const failedHandler = makeSyncWsHandler(fixture.deps, { keepaliveMs: 60_000 });
    const failed = makeV2Socket({ tabId: "failed-tab", sendResult: 0 });
    failedHandler.open(failed.ws);
    expect(fixture.deps.uiLayoutApplies.stats().targets).toBe(0);

    const handler = makeSyncWsHandler(fixture.deps, { keepaliveMs: 60_000 });
    const writable = makeV2Socket({ tabId: "writable-tab" });
    handler.open(writable.ws);
    stopKeepalive(writable.socket);
    expect(fixture.deps.uiLayoutApplies.stats().targets).toBe(1);
    handler.close(writable.ws);
    expect(fixture.deps.uiLayoutApplies.stats().targets).toBe(0);

    for (const candidate of [
      makeV2Socket(),
      makeV2Socket({ tabId: "read-only-tab", readOnly: true }),
    ]) {
      handler.open(candidate.ws);
      stopKeepalive(candidate.socket);
      expect(fixture.deps.uiLayoutApplies.stats().targets).toBe(0);
      handler.close(candidate.ws);
    }

    const legacy = new PressureSocket(fixture, 1, 0, false);
    legacy.data.tabId = "legacy-tab";
    legacy.data.viewerKey = `${fixture.fingerprint}:legacy-tab`;
    handler.open(legacy as unknown as ServerWebSocket<SyncWsData>);
    stopKeepalive(legacy);
    expect(fixture.deps.uiLayoutApplies.stats().targets).toBe(0);
    handler.close(legacy as unknown as ServerWebSocket<SyncWsData>);
  });

  test("a published apply is delivered only to its exact target socket", async () => {
    const handler = makeSyncWsHandler(fixture.deps, { keepaliveMs: 60_000 });
    const target = makeV2Socket({ tabId: "target-tab" });
    const peer = makeV2Socket({ tabId: "peer-tab" });
    handler.open(target.ws);
    handler.open(peer.ws);
    stopKeepalive(target.socket);
    stopKeepalive(peer.socket);
    target.socket.frames.length = 0;
    peer.socket.frames.length = 0;

    let publication: UiLayoutApplyPublication | undefined;
    const pending = fixture.deps.uiLayoutApplies.requestApply(
      dashboardId,
      fixture.fingerprint,
      "target-tab",
      new AbortController().signal,
      (published) => {
        publication = published;
        publishApply(published);
      },
    );
    if (!publication) throw new Error("targeted apply was not published");
    const targetFrame = target.socket.frames.find((frame) => frame.frame.case === "uiCommand");
    expect(targetFrame?.frame).toMatchObject({
      case: "uiCommand",
      value: {
        targetTabId: "target-tab",
        targetSocketId: target.socket.data.v2!.socketId,
        correlationId: publication.correlationId,
        command: { command: { case: "applyLayout" } },
      },
    });
    expect(peer.socket.frames.some((frame) => frame.frame.case === "uiCommand")).toBe(false);

    sendResult(
      handler,
      target.ws,
      target.socket.data.v2!.socketId,
      publication.correlationId,
    );
    expect((await pending).outcome).toBe(UiApplyLayoutOutcome.APPLIED);
    handler.close(target.ws);
    handler.close(peer.ws);
  });

  test("same tab socket replacement retires the old generation synchronously", async () => {
    const handler = makeSyncWsHandler(fixture.deps, { keepaliveMs: 60_000 });
    const oldSocket = makeV2Socket({ tabId: "replacement-tab" });
    handler.open(oldSocket.ws);
    stopKeepalive(oldSocket.socket);
    const oldPending = fixture.deps.uiLayoutApplies.requestApply(
      dashboardId,
      fixture.fingerprint,
      "replacement-tab",
      new AbortController().signal,
      publishApply,
    );

    const replacement = makeV2Socket({ tabId: "replacement-tab" });
    handler.open(replacement.ws);
    stopKeepalive(replacement.socket);
    expect((await oldPending).outcome).toBe(UiApplyLayoutOutcome.TARGET_GONE);
    handler.close(oldSocket.ws);
    expect(fixture.deps.uiLayoutApplies.stats().targets).toBe(1);

    let replacementCorrelation = "";
    const replacementPending = fixture.deps.uiLayoutApplies.requestApply(
      dashboardId,
      fixture.fingerprint,
      "replacement-tab",
      new AbortController().signal,
      (publication) => {
        replacementCorrelation = publication.correlationId;
        publishApply(publication);
      },
    );
    sendResult(
      handler,
      replacement.ws,
      replacement.socket.data.v2!.socketId,
      replacementCorrelation,
    );
    expect((await replacementPending).outcome).toBe(UiApplyLayoutOutcome.APPLIED);
    handler.close(replacement.ws);
    expect(fixture.deps.uiLayoutApplies.stats().targets).toBe(0);
  });
});

describe("Sync-v2 layout result ingress", () => {
  test("accepts the exact result before terminal readiness and ignores a stale frame socket id", async () => {
    const handler = makeSyncWsHandler(fixture.deps, { keepaliveMs: 60_000 });
    const target = makeV2Socket({ tabId: "pre-ready-tab" });
    handler.open(target.ws);
    stopKeepalive(target.socket);
    const terminal = target.socket.data.v2!.domains.get(SyncDomain.TERMINAL)!;
    expect(terminal.ready).toBe(false);
    const pending = fixture.deps.uiLayoutApplies.requestApply(
      dashboardId,
      fixture.fingerprint,
      "pre-ready-tab",
      new AbortController().signal,
      publishApply,
    );
    const frame = target.socket.frames.findLast((item) => item.frame.case === "uiCommand");
    if (frame?.frame.case !== "uiCommand") throw new Error("apply frame was not sent");

    sendResult(handler, target.ws, "stale-socket", frame.frame.value.correlationId);
    expect(fixture.deps.uiLayoutApplies.stats().pending).toBe(1);
    sendResult(
      handler,
      target.ws,
      target.socket.data.v2!.socketId,
      frame.frame.value.correlationId,
    );
    expect((await pending).outcome).toBe(UiApplyLayoutOutcome.APPLIED);
    handler.close(target.ws);
  });

  test("read-only and unbound v2 sockets cannot settle an apply result", async () => {
    for (const [label, socketOptions] of [
      ["read-only", { tabId: "guarded-tab", readOnly: true }],
      ["unbound", {}],
    ] as const) {
      fixture.deps.uiLayoutApplies.dispose();
      const handler = makeSyncWsHandler(fixture.deps, { keepaliveMs: 60_000 });
      const candidate = makeV2Socket(socketOptions);
      handler.open(candidate.ws);
      stopKeepalive(candidate.socket);
      const syntheticTarget = {
        dashboardId,
        fingerprint: fixture.fingerprint,
        tabId: candidate.socket.data.tabId ?? `synthetic-${label}`,
        socketId: candidate.socket.data.v2!.socketId,
      };
      const unregister = fixture.deps.uiLayoutApplies.registerTarget(syntheticTarget);
      let publishedCorrelation = "";
      const pending = fixture.deps.uiLayoutApplies.requestApply(
        dashboardId,
        syntheticTarget.fingerprint,
        syntheticTarget.tabId,
        new AbortController().signal,
        (publication) => { publishedCorrelation = publication.correlationId; },
      );
      sendResult(
        handler,
        candidate.ws,
        candidate.socket.data.v2!.socketId,
        publishedCorrelation,
      );
      expect(fixture.deps.uiLayoutApplies.stats().pending).toBe(1);
      unregister();
      expect((await pending).outcome).toBe(UiApplyLayoutOutcome.TARGET_GONE);
      handler.close(candidate.ws);
    }
  });

  test("typed state seeds while an earlier apply publication never does", async () => {
    const handler = makeSyncWsHandler(fixture.deps, { keepaliveMs: 60_000 });
    const candidate = makeV2Socket({ tabId: "late-tab" });
    const report = create(UiReportStateRequestSchema, {
      tabId: "reported-tab",
      activePath: "/",
      folderKey: "folder-a",
      layoutDocument: layoutDocument(),
    });
    fixture.deps.uiStates.report({
      dashboardId,
      fingerprint: fixture.fingerprint,
      tabId: "reported-tab",
      state: report,
    });
    uiBus.publish({
      kind: "apply",
      targetTabId: "late-tab",
      targetSocketId: candidate.socket.data.v2!.socketId,
      correlationId: "already-published",
      command: layoutCommand(),
      _dashboard_id: dashboardId,
    });
    handler.open(candidate.ws);
    stopKeepalive(candidate.socket);
    await Promise.resolve();
    const state = candidate.socket.frames.find((frame) => frame.frame.case === "uiState");
    expect(state?.frame).toMatchObject({
      case: "uiState",
      value: {
        state: {
          layoutDocument: {
            schemaVersion: 1,
            focusedLeafKey: "leaf-a",
          },
        },
      },
    });
    expect(candidate.socket.frames.some((frame) => frame.frame.case === "uiCommand")).toBe(false);
    handler.close(candidate.ws);
  });
});
