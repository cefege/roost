// Loopback direct connections must authenticate before registry admission and
// retain an isolated browser namespace only for rolling Ready frames that lack
// both epoch and socket id. Router settlement is mocked at its public seam.

import { afterAll, afterEach, describe, expect, mock, test } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  LocalTerminalClientFrameSchema,
  LocalTerminalServerFrameSchema,
  type LocalTerminalServerFrame,
} from "@roost/protocol/proto/local_terminal_pb";

const retired: unknown[] = [];
mock.module("../src/store/terminal-stream-promotion.ts", () => ({ dispatchDirectTerminalFrame: () => undefined }));
mock.module("../src/ws/terminal-input-router.ts", () => ({
  retireTerminalInput: (token: unknown) => { retired.push(token); },
  settleTerminalInput: () => undefined,
}));
mock.module("../src/ws/terminal-peer.ts", () => ({
  resetTerminalPeerState: () => undefined,
  retireTerminalDirectConnection: () => undefined,
  stageTerminalDirectConnection: () => undefined,
  startTerminalPeerFastPath: () => undefined,
}));
mock.module("../src/store/sync.ts", () => ({ registerSyncV2GenerationHandler: () => undefined }));

// Module evaluation follows mocks because the production module owns lifecycle imports.
const local = await import("../src/ws/local-terminal.ts");

class FakeWebSocket {
  static readonly OPEN = 1;
  readyState = 0;
  binaryType = "";
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  readonly sent: Uint8Array[] = [];
  closed = false;
  constructor(readonly url: string, readonly protocol: string) { sockets.push(this); }
  open(): void { this.readyState = FakeWebSocket.OPEN; this.onopen?.(); }
  send(bytes: Uint8Array): void { this.sent.push(bytes); }
  close(): void { this.closed = true; this.readyState = 3; }
  deliver(frame: LocalTerminalServerFrame["frame"]): void {
    const bytes = toBinary(LocalTerminalServerFrameSchema, create(LocalTerminalServerFrameSchema, { frame }));
    this.onmessage?.({ data: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) });
  }
}
let sockets: FakeWebSocket[] = [];
const globals = globalThis as unknown as { WebSocket: unknown };
const originalWebSocket = globals.WebSocket;
globals.WebSocket = FakeWebSocket;

function grant() {
  return {
    workerFp: "worker-a", grantId: "grant-a", secret: "secret-a", sessionIds: ["session-a"], tabId: "tab-a",
    deviceFingerprint: "device-a", workerEpoch: "epoch-a", peerSupported: true, stunUrls: [], inputRouteSupported: true,
  };
}
function openConnection() {
  let readyCalls = 0;
  const connection = new local.LoopbackTerminalConnection({
    door: { origin: "http://127.0.0.1:4104", workerFingerprint: "worker-a" }, grant: grant(),
    onReady: () => { readyCalls += 1; }, onClosed: () => undefined, onGrantRejected: () => undefined,
  });
  connection.start();
  const socket = sockets.at(-1);
  if (!socket) throw new Error("loopback socket was not created");
  socket.open();
  return { connection, socket, readyCalls: () => readyCalls };
}
function readyFrame(
  overrides: Partial<Extract<LocalTerminalServerFrame["frame"], { case: "ready" }>["value"]> = {},
): Extract<LocalTerminalServerFrame["frame"], { case: "ready" }>["value"] {
  return {
    $typeName: "roost.v1.LocalTerminalReady",
    workerFingerprint: "worker-a", sessionIds: ["session-a"], socketGeneration: 7n,
    workerEpoch: "", socketId: "", peerId: "", ...overrides,
  } as Extract<LocalTerminalServerFrame["frame"], { case: "ready" }>["value"];
}

afterEach(() => { sockets = []; retired.length = 0; });
afterAll(() => { globals.WebSocket = originalWebSocket; });

describe("LoopbackTerminalConnection", () => {
  test("uses a fresh browser namespace only for a complete old Ready tuple", () => {
    const { connection, socket, readyCalls } = openConnection();
    const hello = fromBinary(LocalTerminalClientFrameSchema, socket.sent[0]!);
    expect(hello.frame.case).toBe("hello");
    socket.deliver({ case: "ready", value: readyFrame() });

    const token = connection.token();
    expect(readyCalls()).toBe(1);
    expect(token?.transportKind).toBe("loopback");
    expect(token?.socketId).toMatch(/^[0-9a-f-]{36}$/u);
    expect(token?.processEpoch).toBe(token?.socketId);
    expect(connection.inputRouteSupported).toBe(false);
    connection.close("test close");
    expect(retired).toEqual([token]);
  });

  test("rejects peer Ready fields or a half-present rolling tuple", () => {
    const first = openConnection();
    first.socket.deliver({ case: "ready", value: readyFrame({ workerEpoch: "epoch-a" }) });
    expect(first.connection.token()).toBeNull();
    expect(first.socket.closed).toBe(true);

    const second = openConnection();
    second.socket.deliver({ case: "ready", value: readyFrame({ peerId: "11111111-1111-4111-8111-111111111111" }) });
    expect(second.connection.token()).toBeNull();
    expect(second.socket.closed).toBe(true);
  });

  test("keeps actual worker epoch and socket id on a current Ready tuple", () => {
    const { connection, socket } = openConnection();
    socket.deliver({ case: "ready", value: readyFrame({ workerEpoch: "epoch-a", socketId: "socket-a" }) });
    expect(connection.token()).toMatchObject({ processEpoch: "epoch-a", socketId: "socket-a", workerFp: "worker-a" });
    expect(connection.inputRouteSupported).toBe(true);
  });
});

  test("extends a current grant in place but refuses a narrowed scope", () => {
    const { connection, socket } = openConnection();
    socket.deliver({ case: "ready", value: readyFrame({ workerEpoch: "epoch-a", socketId: "socket-a" }) });
    const token = connection.token();

    expect(connection.updateGrant({ ...grant(), sessionIds: ["session-a", "session-b"] })).toBe(true);
    expect(connection.token()).toBe(token);
    expect(connection.allowsSession("session-b")).toBe(true);
    expect(connection.updateGrant(grant())).toBe(false);
  });

