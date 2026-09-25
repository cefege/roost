// Focused direct attachment receiver tests. They exercise dedicated-grant hello
// admission, synchronous per-chunk receipts, final committed paths, and grant
// expiry semantics without starting a worker, coordinator, or native peer.

import { afterEach, expect, test } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { createHash, randomBytes, randomUUID } from "node:crypto";
import fs from "node:fs";
import {
  ATTACHMENT_TRANSFER_GRANT_TTL_MS,
  ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_WORKER,
  type AttachmentTransferPeerChannelLane,
} from "@roost/protocol/attachment-transfer";
import {
  AttachmentTransferClientFrameSchema,
  AttachmentTransferServerFrameSchema,
  type AttachmentTransferAck,
  type AttachmentTransferServerFrame,
} from "@roost/protocol/proto/attachment_transfer_pb";
import { DLocalAttachmentGrantSchema } from "@roost/protocol/proto/worker_transport_pb";
import { AttachmentDirectSockets } from "../../src/attachments/attachment-direct-socket.ts";
import { AttachmentGrantStore } from "../../src/attachments/attachment-grants.ts";
import { attachmentOperationStatus } from "../../src/attachments/attachment-upload.ts";
import { attachmentSessionDir } from "../../src/attachments/attachment-reaper.ts";
import type {
  AttachmentTransferPort,
  AttachmentTransferSendResult,
} from "../../src/attachments/attachment-transfer-port.ts";

const DEVICE = "a".repeat(64);
const WORKER_FP = "b".repeat(64);
const cleanups: Array<() => void> = [];

afterEach(() => {
  for (const cleanup of cleanups.splice(0)) cleanup();
});

class FakeLoopbackPort implements AttachmentTransferPort {
  readonly kind = "loopback" as const;
  readonly socketId = randomUUID();
  readonly frames: Uint8Array[] = [];
  readonly closeReasons: string[] = [];
  private live = true;

  get open(): boolean {
    return this.live;
  }

  send(bytes: Uint8Array, _lane: AttachmentTransferPeerChannelLane): AttachmentTransferSendResult {
    if (!this.live) return "refused";
    this.frames.push(new Uint8Array(bytes));
    return "accepted";
  }

  closeAfterDrain(reason: string): void {
    this.close(1000, reason);
  }

  markAuthenticated(): void {}

  close(_code?: number, reason?: string): void {
    this.live = false;
    this.closeReasons.push(reason ?? "");
  }
}

interface DirectFixture {
  readonly sessionId: string;
  readonly uploadId: string;
  readonly filename: string;
  readonly totalBytes: number;
  readonly secret: string;
  readonly grantId: string;
  readonly workerEpoch: string;
  readonly grants: AttachmentGrantStore;
  readonly sockets: AttachmentDirectSockets;
  open(): FakeLoopbackPort;
}

function createFixture(totalBytes: number, now?: () => number): DirectFixture {
  const sessionId = `test-direct-${randomUUID()}`;
  const uploadId = randomUUID();
  const filename = "payload.bin";
  const secret = randomBytes(32).toString("hex");
  const grantId = randomUUID();
  const workerEpoch = randomUUID();
  const grants = new AttachmentGrantStore({ workerEpoch, ...(now ? { now } : {}) });
  grants.install(create(DLocalAttachmentGrantSchema, {
    requestId: randomUUID(),
    grantId,
    secretSha256: createHash("sha256").update(secret).digest("hex"),
    sessionId,
    uploadId,
    filename,
    shortPath: false,
    totalBytes: BigInt(totalBytes),
    deviceFingerprint: DEVICE,
    tabId: randomUUID(),
    ttlMs: ATTACHMENT_TRANSFER_GRANT_TTL_MS,
    workerEpoch,
  }));
  const sockets = new AttachmentDirectSockets({
    grants,
    workerFingerprint: WORKER_FP,
    workerEpoch,
  });
  cleanups.push(() => {
    sockets.dispose();
    grants.dispose();
    fs.rmSync(attachmentSessionDir(sessionId), { recursive: true, force: true });
  });
  return {
    sessionId,
    uploadId,
    filename,
    totalBytes,
    secret,
    grantId,
    workerEpoch,
    grants,
    sockets,
    open: () => {
      const port = new FakeLoopbackPort();
      sockets.onOpen(port);
      return port;
    },
  };
}

function sendHello(fixture: DirectFixture, port: FakeLoopbackPort, secret = fixture.secret): void {
  fixture.sockets.onMessage(port, toBinary(AttachmentTransferClientFrameSchema, create(
    AttachmentTransferClientFrameSchema,
    {
      frame: {
        case: "hello",
        value: {
          grantId: fixture.grantId,
          secret,
          tabId: fixture.grants.current(fixture.grantId)?.tabId ?? "",
          deviceFingerprint: DEVICE,
          sessionId: fixture.sessionId,
          uploadId: fixture.uploadId,
          filename: fixture.filename,
          shortPath: false,
          totalBytes: BigInt(fixture.totalBytes),
          peerId: "",
          workerEpoch: fixture.workerEpoch,
        },
      },
    },
  )));
}

async function sendChunk(
  fixture: DirectFixture,
  port: FakeLoopbackPort,
  seq: number,
  offset: number,
  data: Uint8Array,
  last: boolean,
): Promise<string> {
  const chunkSha256 = new Bun.CryptoHasher("sha256").update(data).digest("hex");
  await fixture.sockets.onMessage(port, toBinary(AttachmentTransferClientFrameSchema, create(
    AttachmentTransferClientFrameSchema,
    {
      frame: {
        case: "chunk",
        value: {
          uploadId: fixture.uploadId,
          seq,
          offset: BigInt(offset),
          data,
          last,
          chunkSha256,
        },
      },
    },
  )));
  return chunkSha256;
}

function decodedFrames(port: FakeLoopbackPort): AttachmentTransferServerFrame[] {
  return port.frames.map((bytes) => fromBinary(AttachmentTransferServerFrameSchema, bytes));
}

function acknowledgements(port: FakeLoopbackPort): AttachmentTransferAck[] {
  const values: AttachmentTransferAck[] = [];
  for (const frame of decodedFrames(port)) {
    if (frame.frame.case === "ack") values.push(frame.frame.value);
  }
  return values;
}

test("rejects a direct hello whose dedicated secret does not match", () => {
  const fixture = createFixture(1);
  const port = fixture.open();

  sendHello(fixture, port, "wrong-secret");

  expect(port.open).toBe(false);
  expect(decodedFrames(port).map((frame) => frame.frame.case)).toEqual(["closed"]);
  expect(decodedFrames(port)[0]!.frame.value).toMatchObject({ reason: "grant_unavailable" });
  expect(fs.existsSync(attachmentSessionDir(fixture.sessionId))).toBe(false);
});

test("writes exact bytes then ACKs each chunk and returns the committed path", async () => {
  const fixture = createFixture(5);
  const port = fixture.open();
  sendHello(fixture, port);
  const firstDigest = await sendChunk(fixture, port, 0, 0, Uint8Array.of(1, 2), false);
  const finalDigest = await sendChunk(fixture, port, 1, 2, Uint8Array.of(3, 4, 5), true);

  const acks = acknowledgements(port);
  expect(acks).toHaveLength(2);
  expect(acks[0]).toMatchObject({
    uploadId: fixture.uploadId,
    seq: 0,
    bytesReceived: 2n,
    absPath: "",
    error: "",
    chunkSha256: firstDigest,
  });
  const finalAck = acks[1]!;
  expect(finalAck).toMatchObject({
    uploadId: fixture.uploadId,
    seq: 1,
    bytesReceived: 5n,
    error: "",
    chunkSha256: finalDigest,
  });
  expect(fs.readFileSync(finalAck.absPath)).toEqual(Buffer.from([1, 2, 3, 4, 5]));
  expect(attachmentOperationStatus(fixture.sessionId, fixture.uploadId)).toMatchObject({
    committed: true,
    absPath: finalAck.absPath,
    lastChunkSha256: finalDigest,
  });
  expect(decodedFrames(port).at(-1)!.frame.value).toMatchObject({ reason: "complete" });
});

test("an admitted upload completes after its short grant expires while a new hello is refused", async () => {
  let now = 0;
  const fixture = createFixture(3, () => now);
  const admitted = fixture.open();
  sendHello(fixture, admitted);

  now = ATTACHMENT_TRANSFER_GRANT_TTL_MS + 1;
  expect(fixture.grants.current(fixture.grantId)).toBeNull();
  expect(admitted.open).toBe(true);
  await sendChunk(fixture, admitted, 0, 0, Uint8Array.of(7), false);
  await sendChunk(fixture, admitted, 1, 1, Uint8Array.of(8, 9), true);
  expect(acknowledgements(admitted).at(-1)!.bytesReceived).toBe(3n);

  const fresh = fixture.open();
  sendHello(fixture, fresh);
  expect(fresh.open).toBe(false);
  expect(decodedFrames(fresh)[0]!.frame.value).toMatchObject({ reason: "grant_unavailable" });
});

test("explicit device revocation closes a leased port after grant expiry", () => {
  let now = 0;
  const fixture = createFixture(1, () => now);
  const port = fixture.open();
  sendHello(fixture, port);
  now = ATTACHMENT_TRANSFER_GRANT_TTL_MS + 1;
  expect(fixture.grants.current(fixture.grantId)).toBeNull();

  fixture.sockets.revokeDevice(DEVICE);

  expect(port.open).toBe(false);
  expect(decodedFrames(port).at(-1)!.frame.value).toMatchObject({ reason: "grant_unavailable" });
});

test("a replayed grant cannot admit a second carrier or disturb the admitted upload", async () => {
  const fixture = createFixture(2);
  const admitted = fixture.open();
  sendHello(fixture, admitted);
  await sendChunk(fixture, admitted, 0, 0, Uint8Array.of(1), false);

  const replay = fixture.open();
  sendHello(fixture, replay);
  expect(replay.open).toBe(false);
  expect(decodedFrames(replay).map((frame) => frame.frame.case)).toEqual(["closed"]);

  await sendChunk(fixture, admitted, 1, 1, Uint8Array.of(2), true);
  expect(fs.readFileSync(acknowledgements(admitted).at(-1)!.absPath)).toEqual(Buffer.from([1, 2]));
});

test("an admitted upload frees its pre-hello slot for the next loopback socket", () => {
  const fixture = createFixture(1);
  const idle = Array.from({ length: ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_WORKER }, () => fixture.open());
  expect(idle.every((port) => port.open)).toBe(true);
  expect(fixture.open().open).toBe(false);

  sendHello(fixture, idle[0]!);
  expect(idle[0]!.open).toBe(true);
  expect(fixture.open().open).toBe(true);
});
