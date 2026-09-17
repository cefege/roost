// The local terminal fast path is a second transport for the SAME sessions, so
// the observable contract is: nothing dials without a reachable local worker
// door, the socket authority is that door rather than this page's origin, a
// granted session's input leaves on the local socket and never on Sync, its
// results settle that exact admission, a socket generation boundary fails
// queued batches without replaying them, and inbound cells reach the one
// canonical replica exactly once. The page here is the coordinator's public
// front door, which is the shape that must dial a plaintext loopback door.

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  LocalTerminalClientFrameSchema,
  LocalTerminalServerFrameSchema,
  type LocalTerminalClientFrame,
  type LocalTerminalServerFrame,
} from "@roost/shared/proto/local_terminal_pb";
import { PbCellGridFrameSchema } from "@roost/shared/proto/cell_pb";

interface TestSyncState {
  socketGeneration: number;
  socketId: string;
  processEpoch: string;
  domainGeneration: bigint;
  ready: boolean;
}
interface TestOneof { case: string; value: Record<string, unknown> }

let syncState: TestSyncState | null = {
  socketGeneration: 1,
  socketId: "socket-1",
  processEpoch: "epoch-1",
  domainGeneration: 11n,
  ready: true,
};
const syncSent: TestOneof[] = [];
let door: { origin: string; workerFingerprint: string } | null = null;
let sessions: Record<string, { status: string; worker_fp: string }> = {};
let grantCalls: { sessionIds: string[]; workerFp: string; tabId: string }[] = [];
let grantFails = false;
const cellFrames: string[] = [];
const viewStates: string[] = [];

class FakeWebSocket {
  static readonly OPEN = 1;
  static readonly CLOSED = 3;
  readyState = 0;
  binaryType = "";
  sent: Uint8Array[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  constructor(readonly url: string, readonly protocols?: string | string[]) {
    sockets.push(this);
  }
  send(data: Uint8Array): void { this.sent.push(data); }
  close(): void { this.readyState = FakeWebSocket.CLOSED; }
  /** The worker accepted the upgrade. */
  accept(): void { this.readyState = FakeWebSocket.OPEN; this.onopen?.(); }
  deliver(frame: LocalTerminalServerFrame["frame"]): void {
    const bytes = toBinary(
      LocalTerminalServerFrameSchema,
      create(LocalTerminalServerFrameSchema, { frame }),
    );
    this.onmessage?.({ data: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) });
  }
  /** The socket died without anyone asking. */
  drop(): void { this.readyState = FakeWebSocket.CLOSED; this.onclose?.(); }
  frames(): LocalTerminalClientFrame[] {
    return this.sent.map((bytes) => fromBinary(LocalTerminalClientFrameSchema, bytes));
  }
}
let sockets: FakeWebSocket[] = [];
// The page is the coordinator's HTTPS front door; the socket authority comes
// from the discovered door, never from this document.
const globals = globalThis as unknown as { WebSocket: unknown; location: unknown };
globals.WebSocket = FakeWebSocket;
globals.location = {
  protocol: "https:",
  host: "mic.roost.test",
  origin: "https://mic.roost.test",
};

mock.module("../src/store/sync.ts", () => ({
  currentSyncV2TerminalState: () => syncState,
  sendSyncV2Command: (value: TestOneof) => { syncSent.push(value); return syncState?.ready === true; },
  registerSyncV2ControlHandler: () => () => undefined,
  registerSyncV2GenerationHandler: () => () => undefined,
  requestSyncGenerationRecovery: () => false,
}));
mock.module("../src/store/terminal-stream.ts", () => ({
  dispatchTerminalCellFrame: (pb: { sessionId: string }) => { cellFrames.push(pb.sessionId); },
  dispatchTerminalCellChunk: () => {},
  dispatchTerminalViewState: (frame: { sessionId: string }) => { viewStates.push(frame.sessionId); },
}));
mock.module("../src/lib/localWorkerDiscovery.ts", () => ({
  readLocalWorkerDoor: () => door,
  discoverLocalWorkerDoor: () => {},
  registerLocalWorkerDoorHandler: () => {},
}));
mock.module("../src/lib/diag.ts", () => ({ getSessionTraceId: () => "trace" }));
mock.module("../src/store/root.ts", () => ({ rootStore: { get sessions() { return sessions; } } }));
mock.module("../src/auth/tab-id.ts", () => ({ getTabId: () => "tab-7" }));
mock.module("../src/auth/web-key.ts", () => ({
  getCurrentWebKeyInfo: () => Promise.resolve({ fingerprint: "device-fp", extractable: false }),
}));
mock.module("../src/connect.ts", () => ({
  coordClient: {
    sessionsGrantLocalTerminal: (req: { sessionIds: string[]; workerFp: string; tabId: string }) => {
      grantCalls.push(req);
      if (grantFails) return Promise.reject(new Error("permission denied"));
      return Promise.resolve({ grantId: "grant-1", secret: "s3cret", ttlMs: 43_200_000 });
    },
  },
}));

// Mocks must precede module evaluation; both transports register at load.
const local = await import("../src/ws/local-terminal.ts");
const outbound = await import("../src/ws/sync-outbound.ts");
const transport = await import("../src/store/terminal-stream-transport.ts");
await Promise.resolve();

async function settle(): Promise<void> {
  for (let turn = 0; turn < 8; turn++) await Promise.resolve();
}

/** Drive the production sequence: a pane publishes a view, the coordinator
 * grants, the worker accepts the hello and reports its granted sessions. */
async function establishReadySocket(
  sessionIds = ["s-local"],
  generation = 7n,
): Promise<FakeWebSocket> {
  transport.localTerminalTransport()?.noteViewPublished(sessionIds[0]!);
  await settle();
  const socket = sockets.at(-1);
  if (!socket) throw new Error("local socket was never dialed");
  socket.accept();
  socket.deliver({
    case: "ready",
    value: {
      $typeName: "roost.v1.LocalTerminalReady",
      workerFingerprint: "worker-fp",
      sessionIds,
      socketGeneration: generation,
    },
  });
  return socket;
}

beforeEach(() => {
  local._resetLocalTerminalForTest();
  outbound._resetTerminalOutboundForTest();
  sockets = [];
  syncSent.length = 0;
  cellFrames.length = 0;
  viewStates.length = 0;
  grantCalls = [];
  grantFails = false;
  syncState = {
    socketGeneration: 1,
    socketId: "socket-1",
    processEpoch: "epoch-1",
    domainGeneration: 11n,
    ready: true,
  };
  sessions = {
    "s-local": { status: "open", worker_fp: "worker-fp" },
    "s-remote": { status: "open", worker_fp: "other-fp" },
  };
  door = { origin: "http://127.0.0.1:4104", workerFingerprint: "worker-fp" };
  local.startLocalTerminalFastPath();
});

afterEach(() => {
  local._resetLocalTerminalForTest();
  outbound._resetTerminalOutboundForTest();
});

describe("local terminal fast path", () => {
  test("stays entirely absent when no local worker door is reachable", async () => {
    local._resetLocalTerminalForTest();
    door = null;
    sockets = [];
    local.startLocalTerminalFastPath();
    transport.localTerminalTransport()?.noteViewPublished("s-local");
    await settle();

    expect(sockets).toHaveLength(0);
    expect(grantCalls).toHaveLength(0);
    expect(local.isLocalTerminalSession("s-local")).toBe(false);
    expect(local.localTerminalWorkerFingerprint()).toBe(null);

    const admission = outbound.sendTerminalInput("s-local", new Uint8Array([1]));
    expect(admission.accepted).toBe(true);
    expect(syncSent.map((command) => command.case)).toEqual(["input"]);
  });

  test("routes a granted session to the worker and leaves the rest on Sync", async () => {
    const socket = await establishReadySocket();
    expect(socket.url).toBe("ws://127.0.0.1:4104/ws/local-terminal");
    expect(grantCalls).toEqual([{
      sessionIds: ["s-local"],
      workerFp: "worker-fp",
      tabId: "tab-7",
    }]);
    const hello = socket.frames()[0];
    expect(hello?.frame.case).toBe("hello");
    if (hello?.frame.case !== "hello") throw new Error("hello was never presented");
    expect(hello.frame.value.grantId).toBe("grant-1");
    expect(hello.frame.value.secret).toBe("s3cret");
    expect(hello.frame.value.tabId).toBe("tab-7");
    expect(hello.frame.value.deviceFingerprint).toBe("device-fp");
    expect(local.isLocalTerminalSession("s-local")).toBe(true);
    expect(local.isLocalTerminalSession("s-remote")).toBe(false);

    const localAdmission = outbound.sendTerminalInput("s-local", new TextEncoder().encode("hi"), "view-1");
    const remoteAdmission = outbound.sendTerminalInput("s-remote", new Uint8Array([2]));
    expect(localAdmission.accepted && remoteAdmission.accepted).toBe(true);

    const input = socket.frames()[1];
    if (input?.frame.case !== "input") throw new Error("input did not leave on the local socket");
    expect(input.frame.value.sessionId).toBe("s-local");
    expect(input.frame.value.viewId).toBe("view-1");
    expect([...input.frame.value.data]).toEqual([...new TextEncoder().encode("hi")]);
    expect(syncSent).toHaveLength(1);
    expect(syncSent[0]?.value).toMatchObject({ sessionId: "s-remote" });
  });

  test("settles a rejected batch on the admission that asked for it", async () => {
    const socket = await establishReadySocket();
    const admission = outbound.sendTerminalInput("s-local", new Uint8Array([1, 2, 3]));
    if (!admission.accepted) throw new Error(admission.reason);

    socket.deliver({
      case: "inputRejected",
      value: {
        $typeName: "roost.v1.InputRejected",
        sessionId: "s-local",
        inputSeq: admission.inputSeq,
        domainGeneration: 7n,
        reason: "keeper update preparation blocks terminal writes",
      },
    });

    const outcome = await admission.result;
    expect(outcome.status).toBe("rejected");
    expect(outcome.status === "rejected" && outcome.reason)
      .toBe("keeper update preparation blocks terminal writes");
    expect(outcome.writtenBytes).toBe(0);
  });

  test("fails queued input at a socket generation boundary and never replays it", async () => {
    const socket = await establishReadySocket();
    const admission = outbound.sendTerminalInput("s-local", new Uint8Array([9]));
    if (!admission.accepted) throw new Error(admission.reason);
    expect(socket.frames()[1]?.frame.case).toBe("input");

    expect(transport.localTerminalTransport()?.redial("test generation change")).toBe(true);
    const outcome = await admission.result;
    expect(outcome.status).toBe("ambiguous");
    expect(local.isLocalTerminalSession("s-local")).toBe(false);

    const replacement = sockets.at(-1);
    if (!replacement || replacement === socket) throw new Error("socket was not replaced");
    replacement.accept();
    replacement.deliver({
      case: "ready",
      value: {
        $typeName: "roost.v1.LocalTerminalReady",
        workerFingerprint: "worker-fp",
        sessionIds: ["s-local"],
        socketGeneration: 8n,
      },
    });
    await settle();
    // A new generation may only carry a fresh hello: replayed bytes would be a
    // second write of input the PTY may already have taken.
    expect(replacement.frames().map((frame) => frame.frame.case)).toEqual(["hello"]);

    const result = outbound.sendTerminalInput("s-local", new Uint8Array([10]));
    expect(result.accepted).toBe(true);
    expect(replacement.frames().map((frame) => frame.frame.case)).toEqual(["hello", "input"]);
  });

  test("hands inbound cells to the canonical replica exactly once", async () => {
    const socket = await establishReadySocket();
    socket.deliver({
      case: "cellGrid",
      value: create(PbCellGridFrameSchema, {
        sessionId: "s-local",
        streamId: "11111111-1111-4111-8111-111111111111",
        seq: 1n,
        full: true,
      }),
    });
    socket.deliver({
      case: "terminalViewState",
      value: {
        $typeName: "roost.v1.TerminalViewStateFrame",
        viewId: "22222222-2222-4222-8222-222222222222",
        sessionId: "s-local",
        revision: 1n,
        active: true,
        streamId: "11111111-1111-4111-8111-111111111111",
        status: 1,
        effectiveCols: 80,
        effectiveRows: 24,
        reason: "",
      },
    });

    expect(cellFrames).toEqual(["s-local"]);
    expect(viewStates).toEqual(["s-local"]);
  });

  test("falls back to Sync when the coordinator refuses a grant", async () => {
    grantFails = true;
    transport.localTerminalTransport()?.noteViewPublished("s-local");
    await settle();

    expect(grantCalls).toHaveLength(1);
    expect(sockets).toHaveLength(0);
    expect(local.isLocalTerminalSession("s-local")).toBe(false);
    const admission = outbound.sendTerminalInput("s-local", new Uint8Array([1]));
    expect(admission.accepted).toBe(true);
    expect(syncSent.map((command) => command.case)).toEqual(["input"]);
  });

  test("never asks for a grant covering a session on another worker", async () => {
    transport.localTerminalTransport()?.noteViewPublished("s-remote");
    await settle();

    expect(grantCalls).toHaveLength(0);
    expect(sockets).toHaveLength(0);
  });
});
