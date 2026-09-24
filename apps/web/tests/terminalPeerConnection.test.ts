// Fake browser RTC coverage for the direct peer adapter. It verifies static
// channels, bounded packet framing, authenticated Ready tuple admission and
// cleanup without claiming native/browser interoperability (smoke owns that).

import { afterEach, describe, expect, mock, test } from "bun:test";
import { create, fromBinary, toBinary, type MessageInitShape } from "@bufbuild/protobuf";
import {
  LocalTerminalClientFrameSchema,
  LocalScrollbackResponseSchema,
  LocalTerminalServerFrameSchema,
} from "@roost/protocol/proto/local_terminal_pb";
import {
  InputCommandSchema,
  TerminalViewCommandSchema,
} from "@roost/protocol/proto/sync_pb";
import { encodeTerminalPeerPacket, parseTerminalPeerPacket } from "@roost/protocol/terminal-peer-packets";

const dispatched: unknown[] = [];
const retired: unknown[] = [];
mock.module("../src/store/terminal-stream-promotion.ts", () => ({
  dispatchDirectTerminalFrame: (_token: unknown, frame: unknown) => { dispatched.push(frame); },
}));
mock.module("../src/store/transport/terminal-input-router.ts", () => ({
  retireTerminalInput: (token: unknown) => { retired.push(token); },
}));

// The adapter has browser-only dependencies, so mocks must load before it.
const peer = await import("../src/store/transport/terminal-peer-connection.ts");

const FINGERPRINT = Array.from({ length: 32 }, () => "AA").join(":");
const SDP = [
  "v=0",
  "o=- 1 1 IN IP4 127.0.0.1",
  "s=-",
  "t=0 0",
  "a=ice-ufrag:test",
  "a=ice-pwd:0123456789012345678901",
  `a=fingerprint:sha-256 ${FINGERPRINT}`,
  "m=application 9 UDP/DTLS/SCTP webrtc-datachannel",
  "a=setup:actpass",
  "a=sctp-port:5000",
  "a=max-message-size:65536",
  "a=candidate:1 1 udp 1 192.0.2.1 5000 typ host",
  "",
].join("\r\n");

class FakeDataChannel {
  readyState = "connecting";
  binaryType = "arraybuffer";
  bufferedAmount = 0;
  bufferedAmountLowThreshold = 0;
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onbufferedamountlow: (() => void) | null = null;
  readonly sent: Uint8Array[] = [];
  closed = false;
  constructor(readonly label: string, readonly options: RTCDataChannelInit) {}
  open(): void { this.readyState = "open"; this.onopen?.(); }
  send(bytes: BufferSource): void {
    const view = bytes instanceof ArrayBuffer ? new Uint8Array(bytes) : new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    this.sent.push(new Uint8Array(view));
  }
  receive(bytes: Uint8Array): void { this.onmessage?.({ data: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) }); }
  close(): void { if (this.closed) return; this.closed = true; this.readyState = "closed"; this.onclose?.(); }
}

class FakePeerConnection {
  iceGatheringState = "complete";
  connectionState = "new";
  iceConnectionState = "new";
  localDescription: RTCSessionDescriptionInit | null = null;
  remoteDescription: RTCSessionDescriptionInit | null = null;
  ondatachannel: (() => void) | null = null;
  ontrack: (() => void) | null = null;
  onconnectionstatechange: (() => void) | null = null;
  oniceconnectionstatechange: (() => void) | null = null;
  onicegatheringstatechange: (() => void) | null = null;
  readonly channels: FakeDataChannel[] = [];
  closed = false;
  createDataChannel(label: string, options: RTCDataChannelInit): RTCDataChannel {
    const channel = new FakeDataChannel(label, options);
    this.channels.push(channel);
    return channel as unknown as RTCDataChannel;
  }
  createOffer(): Promise<RTCSessionDescriptionInit> { return Promise.resolve({ type: "offer", sdp: SDP }); }
  setLocalDescription(description: RTCSessionDescriptionInit): Promise<void> { this.localDescription = description; return Promise.resolve(); }
  setRemoteDescription(description: RTCSessionDescriptionInit): Promise<void> { this.remoteDescription = description; return Promise.resolve(); }
  close(): void { this.closed = true; this.connectionState = "closed"; }
}

function grant() {
  return {
    workerFp: "worker-a",
    grantId: "grant-a",
    secret: "secret-a",
    sessionIds: ["session-a"],
    tabId: "tab-a",
    deviceFingerprint: "device-a",
    workerEpoch: "epoch-a",
    peerSupported: true,
    stunUrls: [],
    inputRouteSupported: true,
  };
}
function serverPacket(
  frame: MessageInitShape<typeof LocalTerminalServerFrameSchema>,
  messageId: number,
  lane: "control" | "history" = "control",
): Uint8Array {
  const bytes = toBinary(LocalTerminalServerFrameSchema, create(LocalTerminalServerFrameSchema, frame));
  return encodeTerminalPeerPacket(lane, { messageId, totalBytes: bytes.byteLength, offsetBytes: 0 }, bytes);
}
async function settle(): Promise<void> { for (let turn = 0; turn < 8; turn += 1) await Promise.resolve(); }
function connection(
  fake: FakePeerConnection,
  ready: () => void = () => undefined,
  granted = grant(),
) {
  return peer.TerminalPeerConnection.create({
    workerFp: "worker-a",
    workerEpoch: "epoch-a",
    peerId: "11111111-1111-4111-8111-111111111111",
    grant: granted,
    stunUrls: [],
    hooks: { onReady: ready, onClosed: () => undefined, onInputResult: () => undefined },
  }, { createPeerConnection: () => fake as unknown as RTCPeerConnection });
}

afterEach(() => { dispatched.length = 0; retired.length = 0; });

describe("TerminalPeerConnection", () => {
  test("offers static ordered channels and preserves framed control ordering", async () => {
    const fake = new FakePeerConnection();
    let readyCalls = 0;
    const direct = connection(fake, () => { readyCalls += 1; });
    await expect(direct.createOffer()).resolves.toBe(SDP);
    await direct.acceptAnswer(SDP);
    expect(fake.channels.map((channel) => [channel.label, channel.options.id, channel.options.negotiated, channel.options.ordered]))
      .toEqual([
        ["roost-terminal-control-v1", 0, true, true],
        ["roost-terminal-data-v1", 1, true, true],
        ["roost-terminal-history-v1", 2, true, true],
      ]);

    const control = fake.channels[0]!;
    for (const channel of fake.channels) channel.open();
    await settle();
    const hello = fromBinary(LocalTerminalClientFrameSchema, parseTerminalPeerPacket("control", control.sent[0]!).payload);
    expect(hello.frame.case).toBe("hello");
    control.receive(serverPacket({ frame: { case: "ready", value: {
      workerFingerprint: "worker-a", workerEpoch: "epoch-a", peerId: "11111111-1111-4111-8111-111111111111",
      socketGeneration: 7n, socketId: "socket-a", sessionIds: ["session-a"],
    } } }, 1));
    expect(readyCalls).toBe(1);
    expect(direct.token()).toMatchObject({ transportKind: "webrtc", workerFp: "worker-a", processEpoch: "epoch-a", socketId: "socket-a" });
    expect(direct.telemetry().livenessQualified).toBe(false);
    expect(direct.updateGrant({ ...grant(), sessionIds: ["session-a", "session-b"] })).toBe(true);
    expect(direct.allowsSession("session-b")).toBe(true);
    expect(direct.updateGrant(grant())).toBe(false);

    expect(direct.publishView(create(TerminalViewCommandSchema, {
      sessionId: "session-a", viewId: "22222222-2222-4222-8222-222222222222", cols: 80, rows: 24, revision: 1n, active: true,
    }))).toBe(true);
    expect(direct.sendInput(create(InputCommandSchema, {
      sessionId: "session-a", inputSeq: 1n, data: new Uint8Array(20_000), domainGeneration: 7n, inputRouteEpoch: "",
    }))).toBe("accepted");
    await settle();
    expect(control.sent.slice(1).map((packet) => parseTerminalPeerPacket("control", packet).messageId)).toEqual([2, 3, 3]);
    const scrollback = direct.requestScrollback({
      sessionId: "session-a",
      endRow: 1n,
      maxRows: 1,
      gridEpoch: "grid-a",
    });
    await settle();
    const scrollbackRequest = fromBinary(
      LocalTerminalClientFrameSchema,
      parseTerminalPeerPacket("control", control.sent.at(-1)!).payload,
    );
    expect(scrollbackRequest.frame.case).toBe("scrollback");
    if (scrollbackRequest.frame.case !== "scrollback") throw new Error("scrollback request missing");
    fake.channels[2]!.receive(serverPacket({
      frame: {
        case: "scrollback",
        value: create(LocalScrollbackResponseSchema, {
          requestId: scrollbackRequest.frame.value.requestId,
          gridEpoch: "grid-a",
        }),
      },
    }, 1, "history"));
    await expect(scrollback).resolves.toMatchObject({ gridEpoch: "grid-a" });
    expect(parseTerminalPeerPacket("control", control.sent[3]!).offsetBytes).toBe(16_368);
    const probe = direct.probe("33333333-3333-4333-8333-333333333333");
    control.receive(serverPacket({ frame: { case: "transportProbeResult", value: {
      requestId: "33333333-3333-4333-8333-333333333333", workerFp: "worker-a", workerEpoch: "epoch-a",
    } } }, 2));
    await expect(probe).resolves.toBeUndefined();

    expect(direct.telemetry()).toMatchObject({
      opaquePeerId: "11111111-1111-4111-8111-111111111111",
      bufferedBytes: 0,
    });
    expect(direct.telemetry().lastProbeAtMs).toEqual(expect.any(Number));
    expect(direct.telemetry().rttMs).toEqual(expect.any(Number));
    expect(direct.telemetry().livenessQualified).toBe(true);
    direct.requireFreshProbe();
    expect(direct.telemetry().livenessQualified).toBe(false);
    const staleProbe = direct.probe("44444444-4444-4444-8444-444444444444");
    direct.requireFreshProbe();
    await expect(staleProbe).rejects.toThrow("episode replaced");
    control.receive(serverPacket({ frame: { case: "transportProbeResult", value: {
      requestId: "44444444-4444-4444-8444-444444444444", workerFp: "worker-a", workerEpoch: "epoch-a",
    } } }, 3));
    expect(direct.telemetry().livenessQualified).toBe(false);
    const freshProbe = direct.probe("55555555-5555-4555-8555-555555555555");
    control.receive(serverPacket({ frame: { case: "transportProbeResult", value: {
      requestId: "55555555-5555-4555-8555-555555555555", workerFp: "worker-a", workerEpoch: "epoch-a",
    } } }, 4));
    await expect(freshProbe).resolves.toBeUndefined();
    expect(direct.telemetry().livenessQualified).toBe(true);
  });

  test("accepts a bounded Ready for the full 256-session grant", async () => {
    const fake = new FakePeerConnection();
    const sessionIds = Array.from(
      { length: 256 },
      (_, index) => `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`,
    );
    let ready = false;
    const direct = connection(fake, () => { ready = true; }, {
      ...grant(),
      sessionIds,
    });
    await direct.createOffer();
    await direct.acceptAnswer(SDP);
    for (const channel of fake.channels) channel.open();
    const control = fake.channels[0]!;
    control.receive(serverPacket({
      frame: {
        case: "ready",
        value: {
          workerFingerprint: "worker-a",
          workerEpoch: "epoch-a",
          peerId: "11111111-1111-4111-8111-111111111111",
          socketGeneration: 8n,
          socketId: "socket-max-scope",
          sessionIds,
        },
      },
    }, 1));
    expect(ready).toBe(true);
    expect(direct.allowsSession(sessionIds.at(-1)!)).toBe(true);
  });

  test("rejects a Ready tuple with a different worker epoch", async () => {
    const fake = new FakePeerConnection();
    const direct = connection(fake);
    await direct.createOffer();
    await direct.acceptAnswer(SDP);
    const control = fake.channels[0]!;
    control.open();
    control.receive(serverPacket({ frame: { case: "ready", value: {
      workerFingerprint: "worker-a", workerEpoch: "epoch-b", peerId: "11111111-1111-4111-8111-111111111111",
      socketGeneration: 1n, socketId: "socket-a", sessionIds: ["session-a"],
    } } }, 1));
    expect(direct.token()).toBeNull();
    expect(fake.closed).toBe(true);
  });

  test("rejects an answer without the authenticated SHA-256 fingerprint shape", async () => {
    const fake = new FakePeerConnection();
    const direct = connection(fake);
    await direct.createOffer();
    await expect(direct.acceptAnswer(SDP.replace("sha-256", "sha-1"))).rejects.toThrow("fingerprint");
    direct.close("test cleanup");
  });

  test("closes queues and retires the exact authenticated input generation", async () => {
    const fake = new FakePeerConnection();
    const direct = connection(fake);
    await direct.createOffer();
    await direct.acceptAnswer(SDP);
    const control = fake.channels[0]!;
    control.open();
    control.receive(serverPacket({ frame: { case: "ready", value: {
      workerFingerprint: "worker-a", workerEpoch: "epoch-a", peerId: "11111111-1111-4111-8111-111111111111",
      socketGeneration: 3n, socketId: "socket-a", sessionIds: ["session-a"],
    } } }, 1));
    const token = direct.token();
    direct.close("test close");
    expect(retired).toEqual([token]);
    expect(fake.channels.every((channel) => channel.closed)).toBe(true);
  });
});
