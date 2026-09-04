// Regression: a dropped cell repair and a queued control reply must not
// circularly block each other. The repair leads the reply, preserving the
// authoritative opened → full → RPC ordering without timing out the caller.

import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// Isolate the durable session-event outbox from production worker state.
process.env.ROOST_WORKER_DATA_DIR = mkdtempSync(join(tmpdir(), "coordlink-repair-test-"));

import { expect, test, vi } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { PbCellGridFrameSchema } from "@roost/shared/proto/cell_pb";
import {
  CoordWorkerDownSchema,
  CoordWorkerUpSchema,
  DEventAckSchema,
  DHelloAckSchema,
} from "@roost/shared/proto/worker_transport_pb";
import { WORKER_AUTH_SUBPROTOCOL, type WorkerFp } from "@roost/shared/wire";
import {
  startCoordLink,
  type CoordLink,
  type TransportSendResult,
} from "../src/transport/coord-link.ts";
import { openSessionEventStore } from "../src/transport/session-event-store.ts";
import {
  WS_BUFFERED_HIGH_WATER_BYTES,
  WS_DRAIN_RETRY_MS,
} from "../src/transport/coord-link-constants.ts";

function helloAckBytes(): Uint8Array {
  return toBinary(
    CoordWorkerDownSchema,
    create(CoordWorkerDownSchema, {
      frame: {
        case: "helloAck",
        value: create(DHelloAckSchema, {}),
      },
    }),
  );
}

function eventAckBytes(clientSeq: bigint): Uint8Array {
  return toBinary(CoordWorkerDownSchema, create(CoordWorkerDownSchema, {
    frame: { case: "eventAck", value: create(DEventAckSchema, { clientSeq }) },
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

  constructor(private readonly onSend: (bytes: Uint8Array) => void) {}

  open(): void {
    this.readyState = WebSocket.OPEN;
    this.onopen?.();
  }

  receive(bytes: Uint8Array): void {
    this.onmessage?.({ data: Uint8Array.from(bytes).buffer });
  }

  send(data: unknown): void {
    const bytes = data instanceof ArrayBuffer
      ? new Uint8Array(data)
      : data instanceof Uint8Array
        ? data
        : null;
    if (!bytes) throw new Error("unexpected fake WebSocket payload");
    this.onSend(Uint8Array.from(bytes));
  }

  close(): void {
    if (this.readyState === WebSocket.CLOSED) return;
    this.readyState = WebSocket.CLOSED;
    this.onclose?.();
  }
}

test("pending cell repair drains before a queued scrollback RPC reply", async () => {
  const opened = Promise.withResolvers<void>();
  const socketCreated = Promise.withResolvers<ControlledWebSocket>();
  const sentCases: string[] = [];
  let sentRpcRequestId: string | undefined;
  let sentRpcData: unknown;
  let repairSendResult: TransportSendResult | undefined;
  let dialUrl: string | undefined;
  let snapshotSeq = 0n;
  let dialProtocols: [string, string] | undefined;
  let link!: CoordLink;

  const repair = create(PbCellGridFrameSchema, {
    sessionId: "00000000-0000-4000-8000-000000000777",
    gridEpoch: "repair-grid:0",
    cols: 80,
    rows: 24,
    full: true,
    seq: 1n,
  });
  const scrollbackReply = {
    rows: [],
    cols: 80,
    total: 0,
    start_row: 0,
    end_row: 0,
    grid_epoch: "repair-grid:0",
  };
  vi.useFakeTimers();
  const eventStore = openSessionEventStore({
    dbPath: join(process.env.ROOST_WORKER_DATA_DIR!, "session-event-outbox.sqlite"),
    legacySequencePath: join(process.env.ROOST_WORKER_DATA_DIR!, "client-seq.txt"),
  });

  link = startCoordLink({
    coordHttpUrl: "http://coord.test:4102",
    workerFp: "test-fp" as WorkerFp,
    workerVersion: "test",
    mintJwt: async () => "jwt",
    sessionEventStore: eventStore,
    webSocketFactory: (url, protocols) => {
      dialUrl = url;
      dialProtocols = protocols;
      const controlled = new ControlledWebSocket((bytes) => {
        const frame = fromBinary(CoordWorkerUpSchema, bytes);
        const frameCase = frame.frame.case;
        sentCases.push(frameCase ?? "");
        if (frame.frame.case === "rpcOk") {
          sentRpcRequestId = frame.frame.value.requestId;
          sentRpcData = JSON.parse(frame.frame.value.dataJson) as unknown;
        }
        if (frame.frame.case === "event" && frame.frame.value.event?.kind.case === "snapshot") {
          snapshotSeq = frame.frame.value.clientSeq;
        }
      });
      socketCreated.resolve(controlled);
      return controlled as unknown as WebSocket;
    },
    onOpen: () => opened.resolve(),
    onWritable: () => {
      repairSendResult = link.sendCellGrid(7, repair);
    },
  });
  link.activateSnapshotProvider(() => ({
    kind: "snapshot",
    worker_fp: "test-fp" as WorkerFp,
    sessions: [],
    ts: Date.now(),
  }));

  try {
    const controlled = await socketCreated.promise;
    expect(dialUrl).toBe("ws://coord.test:4102/ws/coord-worker/test-fp");
    expect(dialUrl).not.toContain("jwt");
    expect(dialProtocols).toEqual([WORKER_AUTH_SUBPROTOCOL, "jwt"]);
    controlled.open();
    await opened.promise;
    controlled.receive(helloAckBytes());
    controlled.receive(eventAckBytes(snapshotSeq));

    // First lose a cell to native backpressure, arming an authoritative repair.
    controlled.bufferedAmount = WS_BUFFERED_HIGH_WATER_BYTES;
    expect(link.sendCellGrid(7, repair)).toBe("dropped");

    // The scrollback handler's reply arrives while the socket is still full, so
    // it enters controlPending. This exact combination used to deadlock:
    // maybeNotifyWritable waited for controls, while drainQueues waited for the
    // writable repair before draining controls.
    expect(link.send({
      kind: "rpc-ok",
      request_id: "scrollback-request",
      data: scrollbackReply,
    })).toBe(false);
    expect(sentCases).toEqual(["hello", "event"]);

    controlled.bufferedAmount = 0;
    // Drive the existing 4 ms drain retry deterministically. Under the old
    // circular veto this tick only re-schedules itself and no rpcOk appears.
    vi.advanceTimersByTime(WS_DRAIN_RETRY_MS);

    expect(sentCases).toEqual(["hello", "event", "cellGrid", "rpcOk"]);
    expect(repairSendResult).toBe("sent");
    expect(sentRpcRequestId).toBe("scrollback-request");
    expect(sentRpcData).toEqual(scrollbackReply);
  } finally {
    link.dispose();
    eventStore.close();
    vi.useRealTimers();
  }
});
