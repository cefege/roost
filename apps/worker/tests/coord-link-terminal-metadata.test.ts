// Compact metadata transport and capability-negotiation coverage.
// It verifies coalescing preserves independently changed semantic fields and
// that raw WBinary remains only on an old-coordinator acknowledgement.

import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, vi } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { TERMINAL_METADATA_CAPABILITY } from "@roost/shared/terminal-metadata";
import {
  CoordWorkerDownSchema,
  CoordWorkerUpSchema,
  DEventAckSchema,
  DHelloAckSchema,
} from "@roost/shared/proto/worker_transport_pb";
import type { WorkerFp } from "@roost/shared/wire";
import {
  buildCoordLinkDeps,
  type CoordLinkRefs,
} from "../src/coord-link-deps.ts";
import { startCoordLink } from "../src/transport/coord-link.ts";
import { SessionManager } from "../src/session-manager.ts";
import { createCoordLinkTerminalMetadataOutbox } from "../src/transport/coord-link-terminal-metadata.ts";
import { BACKOFF_INITIAL_MS } from "../src/transport/coord-link-constants.ts";
import { createCoordLinkDownstream } from "../src/transport/coord-link-downstream.ts";
import { openSessionEventStore } from "../src/transport/session-event-store.ts";
import type {
  CoordLink,
  CoordLinkDeps,
  CoordLinkOutbox,
  TerminalMetadataFrame,
} from "../src/transport/coord-link-types.ts";
import { SessionEventTestSink } from "./session-event-test-sink.ts";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

test("coalesces independently changed title and activity into one semantic record", () => {
  const writes: Uint8Array[] = [];
  let scheduled = 0;
  const outbox = createCoordLinkTerminalMetadataOutbox({
    encode: (metadata) => encoder.encode(JSON.stringify(metadata)),
    tryWriteEncoded: (bytes) => {
      writes.push(Uint8Array.from(bytes));
      return true;
    },
    scheduleDrain: () => { scheduled += 1; },
  });
  const title: TerminalMetadataFrame = {
    channelId: 13,
    titleChanged: true,
    title: "compile",
    activityChanged: false,
    activityTsMs: 0,
  };
  const activity: TerminalMetadataFrame = {
    channelId: 13,
    titleChanged: false,
    title: "",
    activityChanged: true,
    activityTsMs: 1_700_000_000_000,
  };

  expect(outbox.send(title, false)).toBe("queued");
  expect(outbox.send(activity, false)).toBe("queued");
  outbox.drain();

  expect(scheduled).toBe(2);
  expect(writes).toHaveLength(1);
  expect(JSON.parse(decoder.decode(writes[0]!))).toEqual({
    channelId: 13,
    titleChanged: true,
    title: "compile",
    activityChanged: true,
    activityTsMs: 1_700_000_000_000,
  });
});

const workerFp = "ef".repeat(32) as WorkerFp;

class ControlledWebSocket {
  binaryType = "blob";
  bufferedAmount = 0;
  readyState: number = WebSocket.CONNECTING;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: ArrayBuffer }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;

  constructor(readonly sent: Uint8Array[]) {}

  open(): void {
    this.readyState = WebSocket.OPEN;
    this.onopen?.();
  }

  receive(bytes: Uint8Array): void {
    this.onmessage?.({ data: Uint8Array.from(bytes).buffer });
  }

  send(data: ArrayBuffer | Uint8Array): void {
    this.sent.push(Uint8Array.from(data instanceof ArrayBuffer ? new Uint8Array(data) : data));
  }

  close(): void {
    if (this.readyState === WebSocket.CLOSED) return;
    this.readyState = WebSocket.CLOSED;
    this.onclose?.();
  }
}

async function settleTransport(): Promise<void> {
  for (let turn = 0; turn < 4; turn += 1) await Promise.resolve();
}

function receiveHelloAck(socket: ControlledWebSocket, capabilities: string[]): void {
  socket.receive(toBinary(CoordWorkerDownSchema, create(CoordWorkerDownSchema, {
    frame: { case: "helloAck", value: create(DHelloAckSchema, { capabilities }) },
  })));
}

function acknowledgeSnapshot(socket: ControlledWebSocket): void {
  const snapshot = socket.sent
    .map((bytes) => fromBinary(CoordWorkerUpSchema, bytes))
    .find((frame) => frame.frame.case === "event");
  if (!snapshot || snapshot.frame.case !== "event") {
    throw new Error("expected snapshot after hello acknowledgement");
  }
  socket.receive(toBinary(CoordWorkerDownSchema, create(CoordWorkerDownSchema, {
    frame: {
      case: "eventAck",
      value: create(DEventAckSchema, { clientSeq: snapshot.frame.value.clientSeq }),
    },
  })));
}

test("ignores a stale hello acknowledgement before changing metadata capability", () => {
  const activeSocket = {} as WebSocket;
  const staleSocket = {} as WebSocket;
  const acceptedModes: boolean[] = [];
  const notifiedModes: boolean[] = [];
  const downstream = createCoordLinkDownstream({
    onHelloAck: (message: {
      reconnected: boolean;
      terminalMetadataNegotiated: boolean;
    }) => {
      notifiedModes.push(message.terminalMetadataNegotiated);
    },
  } as unknown as CoordLinkDeps, {
    send: () => false,
    activeSocket: () => activeSocket,
    acceptHelloAck: (
      _reconnected: boolean,
      terminalMetadataNegotiated?: boolean,
    ) => {
      acceptedModes.push(terminalMetadataNegotiated ?? false);
    },
  } as unknown as CoordLinkOutbox);
  const frame = create(CoordWorkerDownSchema, {
    frame: {
      case: "helloAck",
      value: create(DHelloAckSchema, { capabilities: [TERMINAL_METADATA_CAPABILITY] }),
    },
  });

  downstream.handleDownstream(frame, false, staleSocket);
  expect(acceptedModes).toEqual([]);
  expect(notifiedModes).toEqual([]);

  downstream.handleDownstream(frame, false, activeSocket);
  expect(acceptedModes).toEqual([true]);
  expect(notifiedModes).toEqual([true]);
});

async function createReadyLink(capabilities: string[]) {
  const root = mkdtempSync(join(tmpdir(), "roost-terminal-metadata-link-"));
  const eventStore = openSessionEventStore({
    dbPath: join(root, "outbox.sqlite"),
    legacySequencePath: join(root, "client-seq.txt"),
  });
  const socket = new ControlledWebSocket([]);
  const link = startCoordLink({
    coordHttpUrl: "http://coord.test:4102",
    workerFp,
    workerVersion: "test",
    sessionEventStore: eventStore,
    mintJwt: async () => "jwt",
    webSocketFactory: () => socket as unknown as WebSocket,
  });
  link.activateSnapshotProvider(() => ({
    kind: "snapshot",
    worker_fp: workerFp,
    sessions: [],
    ts: 1_700_000_000_000,
  }));
  await settleTransport();
  socket.open();
  await settleTransport();
  const hello = fromBinary(CoordWorkerUpSchema, socket.sent[0]!);
  receiveHelloAck(socket, capabilities);
  await settleTransport();
  acknowledgeSnapshot(socket);
  await settleTransport();
  return {
    dispose() {
      link.dispose();
      eventStore.close();
      rmSync(root, { force: true, recursive: true });
    },
    hello,
    link,
    socket,
  };
}

test("advertises and uses terminal_metadata_v1 only after the coordinator acknowledges it", async () => {
  const ready = await createReadyLink([TERMINAL_METADATA_CAPABILITY]);
  try {
    expect(ready.hello.frame).toMatchObject({
      case: "hello",
      value: { capabilities: [TERMINAL_METADATA_CAPABILITY] },
    });
    expect(ready.link.sendTerminalMetadata({
      channelId: 21,
      titleChanged: true,
      title: "semantic",
      activityChanged: true,
      activityTsMs: 1_700_000_000_000,
    })).toBe("sent");
    expect(ready.link.sendBinary(21, 0, 1, new Uint8Array([0x1b]))).toBe("dropped");
    expect(ready.socket.sent.map((bytes) => fromBinary(CoordWorkerUpSchema, bytes).frame.case))
      .toEqual(["hello", "event", "terminalMetadata"]);
  } finally {
    ready.dispose();
  }
});

test("falls back to WBinary when the coordinator omits terminal_metadata_v1", async () => {
  const ready = await createReadyLink([]);
  try {
    expect(ready.link.sendTerminalMetadata({
      channelId: 21,
      titleChanged: true,
      title: "semantic",
      activityChanged: false,
      activityTsMs: 0,
    })).toBe("dropped");
    expect(ready.link.sendBinary(21, 0, 1, new Uint8Array([0x1b]))).toBe("sent");
    expect(ready.socket.sent.at(-1) && fromBinary(CoordWorkerUpSchema, ready.socket.sent.at(-1)!).frame.case)
      .toBe("binary");
  } finally {
    ready.dispose();
  }
});

test("replays reconnect-gap raw fallback to an old coordinator then discards it at v1 cutover", async () => {
  vi.useFakeTimers();
  const root = mkdtempSync(join(tmpdir(), "roost-terminal-metadata-reconnect-"));
  const eventStore = openSessionEventStore({
    dbPath: join(root, "outbox.sqlite"),
    legacySequencePath: join(root, "client-seq.txt"),
  });
  let link: CoordLink | null = null;
  const manager = new SessionManager({
    workerFp,
    sink: new SessionEventTestSink(),
    sendBinaryUpstream: (channelId, direction, endSeq, bytes) =>
      link?.sendBinary(channelId, direction, endSeq, bytes) ?? "dropped",
    sendTerminalMetadataUpstream: (metadata) =>
      link?.sendTerminalMetadata(metadata) ?? "dropped",
  });
  manager.sessions.set(21, {} as never);
  const refs: CoordLinkRefs = {
    link: null,
    sessionMgr: manager,
    agentRegistry: null,
    agentDetector: null,
    acquireKeeperUpdateBoundary: null,
  };
  const deps = buildCoordLinkDeps({
    coordHttpUrl: "http://coord.test:4102",
    workerFp,
    mintJwt: async () => "jwt",
    sessionEventStore: eventStore,
    refs,
  });
  const sockets: ControlledWebSocket[] = [];
  deps.webSocketFactory = () => {
    const socket = new ControlledWebSocket([]);
    sockets.push(socket);
    return socket as unknown as WebSocket;
  };
  link = startCoordLink(deps);
  refs.link = link;
  link.activateSnapshotProvider(() => ({
    kind: "snapshot",
    worker_fp: workerFp,
    sessions: [],
    ts: 1_700_000_000_000,
  }));

  try {
    await settleTransport();
    const firstSocket = sockets[0]!;
    firstSocket.open();
    await settleTransport();
    receiveHelloAck(firstSocket, [TERMINAL_METADATA_CAPABILITY]);
    await settleTransport();
    acknowledgeSnapshot(firstSocket);
    await settleTransport();
    expect(manager.terminalMetadataNegotiated).toBe(true);

    firstSocket.close();
    expect(manager.terminalMetadataNegotiated).toBe(false);
    manager._enqueueRawMetadata(21, 1, Buffer.from("old coordinator gap"));
    await settleTransport();

    vi.advanceTimersByTime(BACKOFF_INITIAL_MS);
    await settleTransport();
    const secondSocket = sockets[1]!;
    secondSocket.open();
    receiveHelloAck(secondSocket, []);
    await settleTransport();
    acknowledgeSnapshot(secondSocket);
    await settleTransport();
    const fallback = secondSocket.sent
      .map((bytes) => fromBinary(CoordWorkerUpSchema, bytes))
      .filter((frame) => frame.frame.case === "binary");
    expect(fallback).toHaveLength(1);
    const fallbackFrame = fallback[0];
    if (!fallbackFrame || fallbackFrame.frame.case !== "binary") {
      throw new Error("expected reconnect fallback binary frame");
    }
    expect(Buffer.from(fallbackFrame.frame.value.data).toString()).toBe("old coordinator gap");

    secondSocket.close();
    expect(manager.terminalMetadataNegotiated).toBe(false);
    manager._enqueueRawMetadata(21, 2, Buffer.from("queued before v1"));
    await settleTransport();

    vi.advanceTimersByTime(BACKOFF_INITIAL_MS * 2);
    await settleTransport();
    const thirdSocket = sockets[2]!;
    manager._enqueueRawMetadata(21, 3, Buffer.from("staged before v1"));
    expect(manager.rawMetadataQueues.size).toBe(1);
    thirdSocket.open();
    receiveHelloAck(thirdSocket, [TERMINAL_METADATA_CAPABILITY]);
    expect(manager.terminalMetadataNegotiated).toBe(true);
    expect(manager.rawMetadataQueues.size).toBe(0);
    await settleTransport();
    acknowledgeSnapshot(thirdSocket);
    await settleTransport();
    expect(thirdSocket.sent
      .map((bytes) => fromBinary(CoordWorkerUpSchema, bytes).frame.case)
      .filter((frame) => frame === "binary")).toEqual([]);
  } finally {
    link?.dispose();
    manager.sessions.clear();
    eventStore.close();
    rmSync(root, { force: true, recursive: true });
    vi.useRealTimers();
  }
});
