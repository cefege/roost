import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerDownSchema,
  CoordWorkerUpSchema,
  DEventAckSchema,
  DHelloAckSchema,
  type WSessionEvent,
} from "@roost/shared/proto/worker_transport_pb";
import type { ChannelId, SessionEvent, SessionId, WorkerFp } from "@roost/shared/wire";
import { startCoordLink } from "../src/transport/coord-link.ts";
import { openSessionEventStore } from "../src/transport/session-event-store.ts";

const workerFp = "a".repeat(64) as WorkerFp;
const sessionA = "00000000-0000-4000-8000-000000000001" as SessionId;
const sessionB = "00000000-0000-4000-8000-000000000002" as SessionId;

function downstream(caseName: "helloAck" | "eventAck", clientSeq = 0n): Uint8Array {
  return toBinary(CoordWorkerDownSchema, create(CoordWorkerDownSchema, {
    frame: caseName === "helloAck"
      ? { case: "helloAck", value: create(DHelloAckSchema, {}) }
      : { case: "eventAck", value: create(DEventAckSchema, { clientSeq }) },
  }));
}

class ControlledWebSocket {
  binaryType = "blob";
  bufferedAmount = 0;
  readyState: number = WebSocket.CONNECTING;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: ArrayBuffer }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;

  constructor(readonly sent: Uint8Array[]) {}
  open(): void { this.readyState = WebSocket.OPEN; this.onopen?.(); }
  receive(bytes: Uint8Array): void { this.onmessage?.({ data: Uint8Array.from(bytes).buffer }); }
  send(data: ArrayBuffer | Uint8Array): void {
    this.sent.push(Uint8Array.from(data instanceof ArrayBuffer ? new Uint8Array(data) : data));
  }
  close(): void {
    if (this.readyState === WebSocket.CLOSED) return;
    this.readyState = WebSocket.CLOSED;
    this.onclose?.();
  }
}

function opened(sessionId: SessionId, channel: number): Extract<SessionEvent, { kind: "opened" }> {
  return {
    kind: "opened",
    session_id: sessionId,
    worker_fp: workerFp,
    channel: channel as ChannelId,
    session_kind: "shell",
    cwd: "/tmp",
    ts: 1_700_000_000_000 + channel,
  };
}

function eventFrames(bytes: Uint8Array[]): WSessionEvent[] {
  const events: WSessionEvent[] = [];
  for (const value of bytes) {
    const frame = fromBinary(CoordWorkerUpSchema, value);
    if (frame.frame.case === "event") events.push(frame.frame.value);
  }
  return events;
}

test("hello gates exact-ACK replay, one fresh snapshot, and post-copy traffic across reconnect", async () => {
  const root = mkdtempSync(join(tmpdir(), "roost-reconnect-order-"));
  const store = openSessionEventStore({
    dbPath: join(root, "outbox.sqlite"),
    legacySequencePath: join(root, "client-seq.txt"),
  });
  const firstReservation = store.reserveLifecycleEvent("opened");
  const first = store.appendLifecycleEvent(firstReservation, opened(sessionA, 1));
  // A live record intentionally owns this eventual-close reservation for its
  // whole lifetime. Held capacity must not block the reconnect snapshot.
  const existingLiveClose = store.reserveLifecycleEvent("closed");
  store.holdLifecycleEvent(existingLiveClose);
  const sockets: ControlledWebSocket[] = [];
  let snapshotBuilds = 0;
  let readyEdges = 0;
  const link = startCoordLink({
    coordHttpUrl: "http://coord.test:4102",
    workerFp,
    workerVersion: "test",
    sessionEventStore: store,
    mintJwt: async () => "jwt",
    webSocketFactory: () => {
      const socket = new ControlledWebSocket([]);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    },
    onSnapshotReady: () => { readyEdges += 1; },
  });
  link.activateSnapshotProvider(() => {
    snapshotBuilds += 1;
    return {
      kind: "snapshot",
      worker_fp: workerFp,
      sessions: [{
        id: sessionA,
        worker_fp: workerFp,
        channel: 1 as ChannelId,
        kind: "shell",
        cwd: "/tmp",
        spawn_cwd: "/tmp",
        workspace_id: null,
        status: "open",
        created_at: 1_700_000_000_001,
        closed_at: null,
        custom_title: null,
      }],
      ts: 1_700_000_001_000 + snapshotBuilds,
    };
  });

  try {
    await Promise.resolve();
    await Promise.resolve();
    const firstSocket = sockets[0]!;
    firstSocket.open();
    expect(firstSocket.sent.map((bytes) => fromBinary(CoordWorkerUpSchema, bytes).frame.case)).toEqual(["hello"]);
    link.send({ kind: "rpc-ok", request_id: "held-before-snapshot", data: {} });

    firstSocket.receive(downstream("helloAck"));
    let events = eventFrames(firstSocket.sent);
    expect(events.map((frame) => Number(frame.clientSeq))).toEqual([first.clientSeq]);
    expect(link.protocolPhase()).toBe("replay");

    const secondEvent: Extract<SessionEvent, { kind: "closed" }> = {
      kind: "closed", session_id: sessionA, exit_code: 0, ts: 1_700_000_000_100,
    };
    const secondReservation = store.reserveLifecycleEvent("closed");
    const second = store.appendLifecycleEvent(secondReservation, secondEvent);
    link.send({ kind: "event", event: secondEvent, clientSeq: second.clientSeq, eventClass: "lifecycle" });

    firstSocket.receive(downstream("eventAck", BigInt(second.clientSeq)));
    expect(eventFrames(firstSocket.sent)).toHaveLength(1);
    firstSocket.receive(downstream("eventAck", BigInt(first.clientSeq)));
    events = eventFrames(firstSocket.sent);
    expect(events.map((frame) => Number(frame.clientSeq))).toEqual([first.clientSeq, second.clientSeq]);

    // A lifecycle mutation that reserved capacity before its async keeper work
    // must block the snapshot copy even though no durable row exists yet.
    const startedMutation = store.reserveLifecycleEvent("opened");
    firstSocket.receive(downstream("eventAck", BigInt(second.clientSeq)));
    expect(eventFrames(firstSocket.sent)).toHaveLength(2);
    expect(link.protocolPhase()).toBe("replay");
    store.releaseLifecycleEvent(startedMutation);
    link.snapshotStateChanged();
    events = eventFrames(firstSocket.sent);
    const snapshotFrame = events.at(-1)!;
    expect(snapshotFrame.event?.kind.case).toBe("snapshot");
    const snapshotSeq = snapshotFrame.clientSeq;
    expect(link.protocolPhase()).toBe("snapshot");
    expect(link.ready()).toBe(false);

    const postCopyEvent = opened(sessionB, 2);
    const postCopyReservation = store.reserveLifecycleEvent("opened");
    const postCopy = store.appendLifecycleEvent(postCopyReservation, postCopyEvent);
    link.send({ kind: "event", event: postCopyEvent, clientSeq: postCopy.clientSeq, eventClass: "lifecycle" });
    const metadataSeq = store.nextClientSeq();
    link.send({
      kind: "event",
      event: { kind: "cwd", session_id: sessionB, cwd: "/next", ts: 1_700_000_000_200 },
      clientSeq: metadataSeq,
      eventClass: "metadata",
      metadataKey: `${sessionB}:cwd`,
    });
    expect(eventFrames(firstSocket.sent).at(-1)!.clientSeq).toBe(snapshotSeq);

    firstSocket.receive(downstream("eventAck", snapshotSeq));
    expect(link.ready()).toBe(true);
    expect(readyEdges).toBe(1);
    events = eventFrames(firstSocket.sent);
    expect(events.at(-1)!.clientSeq).toBe(BigInt(postCopy.clientSeq));
    expect(events.filter((frame) => frame.clientSeq === BigInt(metadataSeq))).toHaveLength(0);

    firstSocket.receive(downstream("eventAck", BigInt(postCopy.clientSeq)));
    expect(eventFrames(firstSocket.sent).at(-1)!.clientSeq).toBe(BigInt(metadataSeq));

    link.relocate("http://coord.test:4102", true);
    expect(link.ready()).toBe(false);
    await Promise.resolve();
    await Promise.resolve();
    const secondSocket = sockets[1]!;
    secondSocket.open();
    expect(secondSocket.sent.map((bytes) => fromBinary(CoordWorkerUpSchema, bytes).frame.case)).toEqual(["hello"]);
    secondSocket.receive(downstream("helloAck"));
    const regenerated = eventFrames(secondSocket.sent);
    expect(regenerated).toHaveLength(1);
    expect(regenerated[0]!.event?.kind.case).toBe("snapshot");
    expect(snapshotBuilds).toBe(2);
  } finally {
    link.dispose();
    store.releaseLifecycleEvent(existingLiveClose);
    store.close();
  }
});

test("oversized snapshots stay unready and become eligible after membership and metadata shrink", async () => {
  const root = mkdtempSync(join(tmpdir(), "roost-snapshot-cap-"));
  const store = openSessionEventStore({
    dbPath: join(root, "outbox.sqlite"),
    legacySequencePath: join(root, "client-seq.txt"),
  });
  const sockets: ControlledWebSocket[] = [];
  let mode: "members" | "bytes" | "eligible" = "members";
  const link = startCoordLink({
    coordHttpUrl: "http://coord.test:4102",
    workerFp,
    workerVersion: "test",
    sessionEventStore: store,
    mintJwt: async () => "jwt",
    webSocketFactory: () => {
      const socket = new ControlledWebSocket([]);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    },
  });
  link.activateSnapshotProvider(() => {
    const count = mode === "members" ? 1_025 : 1;
    return {
      kind: "snapshot",
      worker_fp: workerFp,
      ts: 1_700_000_002_000,
      sessions: Array.from({ length: count }, (_, index) => ({
        id: `00000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}` as SessionId,
        worker_fp: workerFp,
        channel: (index + 1) as ChannelId,
        kind: "shell" as const,
        cwd: mode === "bytes" ? "x".repeat(4 * 1024 * 1024) : "/tmp",
        spawn_cwd: "/tmp",
        workspace_id: null,
        status: "open" as const,
        created_at: 1_700_000_000_000,
        closed_at: null,
        custom_title: null,
      })),
    };
  });
  try {
    await Promise.resolve();
    await Promise.resolve();
    const socket = sockets[0]!;
    socket.open();
    socket.receive(downstream("helloAck"));
    expect(eventFrames(socket.sent)).toHaveLength(0);
    expect(link.protocolPhase()).toBe("snapshot");
    expect(link.ready()).toBe(false);

    mode = "bytes";
    link.snapshotStateChanged();
    expect(eventFrames(socket.sent)).toHaveLength(0);
    expect(link.ready()).toBe(false);

    mode = "eligible";
    link.snapshotStateChanged();
    const snapshot = eventFrames(socket.sent);
    expect(snapshot).toHaveLength(1);
    socket.receive(downstream("eventAck", snapshot[0]!.clientSeq));
    expect(link.ready()).toBe(true);
  } finally {
    link.dispose();
    store.close();
  }
});
