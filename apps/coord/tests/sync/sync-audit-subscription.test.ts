// Covers the lazy audit listener boundary for Sync v2 and legacy Sync v1.
// The real socket handler verifies command-to-feed ordering and cleanup.
// Direct feed cases pin install-wide authorization and unchanged V1 semantics.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { ServerWebSocket } from "bun";
import { create, toBinary } from "@bufbuild/protobuf";
import {
  SyncClientFrameSchema,
  SyncDomain,
  SyncDomainReadyCommandSchema,
  SyncDomainSubscriptionCommandSchema,
  type FirehoseFrame,
  type SyncClientFrame,
} from "@roost/protocol/proto/sync_pb";
import { auditBus } from "../../src/events/buses.ts";
import {
  startSyncFeed,
  type SyncResourceIndex,
} from "../../src/sync/sync-feed.ts";
import { makeSyncWsHandler, type SyncWsData } from "../../src/sync/sync-ws-handler.ts";
import { createSyncV2SocketState } from "../../src/sync/sync-ws-v2-state.ts";
import {
  createSyncWsKeepaliveCoordFixture,
  type SyncWsKeepaliveCoordFixture,
} from "./sync-ws-keepalive-coord-fixture.ts";
import { PressureSocket } from "./sync-ws-keepalive-pressure-fixture.ts";

let fixture: SyncWsKeepaliveCoordFixture;

beforeAll(async () => {
  fixture = await createSyncWsKeepaliveCoordFixture();
});

afterAll(async () => {
  await fixture?.close();
});

function installWideScope(): SyncResourceIndex {
  return {
    ownerWorkerFp: null,
    workerFps: new Set(),
    sessionIds: new Set(),
    workspaceIds: new Set(),
  };
}

function workerScope(): SyncResourceIndex {
  return {
    ownerWorkerFp: "worker-only",
    workerFps: new Set(["worker-only"]),
    sessionIds: new Set(),
    workspaceIds: new Set(),
  };
}

function publishAudit(id: number, path: string): void {
  auditBus.publish({
    id,
    ts: Date.now(),
    caller_fp: null,
    caller_label: null,
    method: "POST",
    path,
    status: 200,
    trace_id: null,
  });
}

function sendV2Command(
  handler: ReturnType<typeof makeSyncWsHandler>,
  ws: ServerWebSocket<SyncWsData>,
  socketId: string,
  command: SyncClientFrame["command"],
): void {
  handler.message(ws, toBinary(SyncClientFrameSchema, create(SyncClientFrameSchema, {
    socketId,
    command,
  })) as unknown as Buffer);
}

async function flushSyncCommands(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

function openV2Socket() {
  const socket = new PressureSocket(fixture, 1, 0, true);
  socket.data.v2 = createSyncV2SocketState();
  const ws = socket as unknown as ServerWebSocket<SyncWsData>;
  const handler = makeSyncWsHandler(fixture.deps, { keepaliveMs: 60_000 });
  handler.open(ws);
  clearInterval(socket.data.keepaliveTimer ?? undefined);
  socket.data.keepaliveTimer = null;
  return { handler, socket, ws };
}

test("Sync v2 subscribes to audit only on command and holds live rows through the snapshot barrier", async () => {
  const beforeSubscribers = auditBus.subscriberCount;
  const { handler, socket, ws } = openV2Socket();
  try {
    const v2 = socket.data.v2!;
    const audit = v2.domains.get(SyncDomain.AUDIT)!;
    expect(auditBus.subscriberCount).toBe(beforeSubscribers);

    socket.data.feed!.setDomainSubscribed(SyncDomain.TERMINAL, true);
    expect(auditBus.subscriberCount).toBe(beforeSubscribers);

    sendV2Command(handler, ws, v2.socketId, {
      case: "domainSubscribe",
      value: create(SyncDomainSubscriptionCommandSchema, {
        domain: SyncDomain.AUDIT,
        generation: audit.generation,
      }),
    });
    expect(auditBus.subscriberCount).toBe(beforeSubscribers + 1);
    expect(audit).toMatchObject({ subscribed: true, ready: false });

    publishAudit(8_001, "/sync-audit/discarded-before-ready");
    await flushSyncCommands();
    expect(audit.queue.map((item) => item.frame.frame.case)).toEqual(["auditRow"]);
    expect(socket.frames.some((frame) => frame.frame.case === "auditRow")).toBe(false);

    sendV2Command(handler, ws, v2.socketId, {
      case: "domainUnsubscribe",
      value: create(SyncDomainSubscriptionCommandSchema, {
        domain: SyncDomain.AUDIT,
        generation: audit.generation,
      }),
    });
    expect(auditBus.subscriberCount).toBe(beforeSubscribers);
    expect(audit).toMatchObject({ subscribed: false, ready: false });
    expect(audit.queue).toEqual([]);
    await flushSyncCommands();
    expect(socket.frames.some((frame) => frame.frame.case === "auditRow")).toBe(false);

    sendV2Command(handler, ws, v2.socketId, {
      case: "domainSubscribe",
      value: create(SyncDomainSubscriptionCommandSchema, {
        domain: SyncDomain.AUDIT,
        generation: audit.generation,
      }),
    });
    expect(auditBus.subscriberCount).toBe(beforeSubscribers + 1);

    publishAudit(8_002, "/sync-audit/buffered");
    await flushSyncCommands();
    expect(audit.queue.map((item) => item.frame.frame.case)).toEqual(["auditRow"]);

    sendV2Command(handler, ws, v2.socketId, {
      case: "domainReady",
      value: create(SyncDomainReadyCommandSchema, {
        domain: SyncDomain.AUDIT,
        generation: audit.generation,
      }),
    });
    await flushSyncCommands();
    expect(socket.frames.filter((frame) => frame.frame.case === "auditRow").map((frame) =>
      frame.frame.case === "auditRow" ? frame.frame.value.id : 0n,
    )).toEqual([8_002n]);

    sendV2Command(handler, ws, v2.socketId, {
      case: "domainSubscribe",
      value: create(SyncDomainSubscriptionCommandSchema, {
        domain: SyncDomain.AUDIT,
        generation: audit.generation,
      }),
    });
    expect(audit).toMatchObject({ subscribed: true, ready: true });
    expect(auditBus.subscriberCount).toBe(beforeSubscribers + 1);

    sendV2Command(handler, ws, v2.socketId, {
      case: "domainUnsubscribe",
      value: create(SyncDomainSubscriptionCommandSchema, {
        domain: SyncDomain.AUDIT,
        generation: audit.generation,
      }),
    });
    expect(auditBus.subscriberCount).toBe(beforeSubscribers);
    expect(audit).toMatchObject({ subscribed: false, ready: false });
    expect(audit.queue).toEqual([]);

    publishAudit(8_003, "/sync-audit/after-unsubscribe");
    await flushSyncCommands();
    expect(socket.frames.filter((frame) => frame.frame.case === "auditRow")).toHaveLength(1);
  } finally {
    handler.close(ws);
  }
  expect(auditBus.subscriberCount).toBe(beforeSubscribers);
});

test("Sync v2 socket disposal releases an active audit listener", () => {
  const beforeSubscribers = auditBus.subscriberCount;
  const { handler, socket, ws } = openV2Socket();
  let closed = false;
  try {
    const v2 = socket.data.v2!;
    const audit = v2.domains.get(SyncDomain.AUDIT)!;
    sendV2Command(handler, ws, v2.socketId, {
      case: "domainSubscribe",
      value: create(SyncDomainSubscriptionCommandSchema, {
        domain: SyncDomain.AUDIT,
        generation: audit.generation,
      }),
    });
    expect(auditBus.subscriberCount).toBe(beforeSubscribers + 1);

    handler.close(ws);
    closed = true;
    expect(auditBus.subscriberCount).toBe(beforeSubscribers);
  } finally {
    if (!closed) handler.close(ws);
  }
});

test("worker-scoped Sync v2 feeds cannot subscribe to install-wide audits", () => {
  const beforeSubscribers = auditBus.subscriberCount;
  const frames: FirehoseFrame[] = [];
  const feed = startSyncFeed(
    fixture.deps,
    workerScope(),
    0,
    (frame) => { frames.push(frame); },
    null,
    false,
    { version: 2, socketId: "worker-sync", onRecoveryReset: () => {} },
  );
  try {
    feed.setDomainSubscribed(SyncDomain.AUDIT, true);
    expect(auditBus.subscriberCount).toBe(beforeSubscribers);

    publishAudit(8_003, "/sync-audit/worker");
    expect(frames.some((frame) => frame.frame.case === "auditRow")).toBe(false);
  } finally {
    feed.dispose();
  }
  expect(auditBus.subscriberCount).toBe(beforeSubscribers);
});

test("Sync v1 retains eager install-wide audit delivery", () => {
  const beforeSubscribers = auditBus.subscriberCount;
  const frames: FirehoseFrame[] = [];
  const feed = startSyncFeed(
    fixture.deps,
    installWideScope(),
    0,
    (frame) => { frames.push(frame); },
    null,
    false,
  );
  try {
    expect(auditBus.subscriberCount).toBe(beforeSubscribers + 1);
    feed.setDomainSubscribed(SyncDomain.AUDIT, false);
    expect(auditBus.subscriberCount).toBe(beforeSubscribers + 1);

    publishAudit(8_004, "/sync-audit/v1");
    expect(frames.filter((frame) => frame.frame.case === "auditRow").map((frame) =>
      frame.frame.case === "auditRow" ? frame.frame.value.id : 0n,
    )).toEqual([8_004n]);
  } finally {
    feed.dispose();
  }
  expect(auditBus.subscriberCount).toBe(beforeSubscribers);
});
