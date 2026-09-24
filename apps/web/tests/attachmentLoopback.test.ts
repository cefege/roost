// Fake-loopback coverage for the attachment protocol's authenticated byte transport.
// It decodes actual protobuf frames and ACKs each serialized direct File slice.
// Browser/worker interoperability remains the integration tier; this pins adapter semantics.

import { describe, expect, test } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES } from "@roost/protocol/attachment-transfer";
import {
  AttachmentTransferAckSchema,
  AttachmentTransferClientFrameSchema,
  AttachmentTransferReadySchema,
  AttachmentTransferServerFrameSchema,
  AttachmentTransferStatusSchema,
  type AttachmentTransferClientFrame,
  type AttachmentTransferServerFrame,
} from "@roost/protocol/proto/attachment_transfer_pb";
import type { AttachmentDirectGrant } from "../src/client/attachments/attachmentDirectGrant.ts";
import { openAttachmentLoopbackTransfer } from "../src/client/carriers/attachment-loopback.ts";
import { sendAttachmentFile } from "../src/client/attachments/attachmentTransfer.ts";

class FakeSocket {
  binaryType = "blob";
  readyState = 0;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  closed = false;
  readonly sent: Uint8Array[] = [];
  onClientFrame: ((frame: AttachmentTransferClientFrame["frame"]) => void) | null = null;

  constructor(readonly url: string, readonly protocol: string) {}

  open(): void {
    this.readyState = 1;
    this.onopen?.();
  }

  send(data: unknown): void {
    const bytes = data instanceof Uint8Array ? data : new Uint8Array(data as ArrayBuffer);
    this.sent.push(new Uint8Array(bytes));
    this.onClientFrame?.(fromBinary(AttachmentTransferClientFrameSchema, bytes).frame);
  }

  deliver(frame: AttachmentTransferServerFrame["frame"]): void {
    const bytes = toBinary(AttachmentTransferServerFrameSchema, create(AttachmentTransferServerFrameSchema, { frame }));
    this.onmessage?.({ data: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) });
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.readyState = 3;
    this.onclose?.();
  }
}

function grant(): AttachmentDirectGrant {
  return {
    workerFp: "worker-a",
    sessionId: "session-a",
    uploadId: "upload-a",
    filename: "direct.bin",
    shortPath: false,
    totalBytes: ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES + 5,
    grantId: "grant-a",
    secret: "secret-a",
    tabId: "tab-a",
    deviceFingerprint: "device-a",
    workerEpoch: "epoch-a",
    peerSupported: true,
    stunUrls: [],
  };
}

describe("openAttachmentLoopbackTransfer", () => {
  test("authenticates then sends direct bytes in order and advances progress from ACKs", async () => {
    const directGrant = grant();
    const sockets: FakeSocket[] = [];
    const chunks: Array<{ seq: number; offset: bigint; data: Uint8Array; last: boolean; chunkSha256: string }> = [];
    let received = 0;
    const opening = openAttachmentLoopbackTransfer(
      { door: { origin: "http://127.0.0.1:4104", workerFingerprint: "worker-a" }, grant: directGrant },
      { createSocket: (url, protocol) => {
        const socket = new FakeSocket(url, protocol);
        sockets.push(socket);
        return socket as unknown as WebSocket;
      } },
    );
    const socket = sockets.at(0);
    if (!socket) throw new Error("loopback socket was not created");
    socket.onClientFrame = (frame) => {
      if (frame.case === "hello") {
        expect(frame.value).toMatchObject({
          grantId: directGrant.grantId,
          secret: directGrant.secret,
          tabId: directGrant.tabId,
          deviceFingerprint: directGrant.deviceFingerprint,
          sessionId: directGrant.sessionId,
          uploadId: directGrant.uploadId,
          filename: directGrant.filename,
          shortPath: directGrant.shortPath,
          totalBytes: BigInt(directGrant.totalBytes),
          workerEpoch: directGrant.workerEpoch,
          peerId: "",
        });
        socket.deliver({
          case: "ready",
          value: create(AttachmentTransferReadySchema, {
            workerFingerprint: "worker-a",
            workerEpoch: "epoch-a",
            sessionId: "session-a",
            uploadId: "upload-a",
          }),
        });
        return;
      }
      if (frame.case !== "chunk") throw new Error("expected attachment chunk");
      const chunk = frame.value;
      chunks.push({ ...chunk, data: new Uint8Array(chunk.data) });
      received += chunk.data.byteLength;
      socket.deliver({
        case: "ack",
        value: create(AttachmentTransferAckSchema, {
          uploadId: chunk.uploadId,
          seq: chunk.seq,
          bytesReceived: BigInt(received),
          absPath: chunk.last ? "/worker/direct.bin" : "",
          error: "",
          chunkSha256: chunk.chunkSha256,
        }),
      });
    };
    socket.open();
    const connection = await opening;
    const bytes = new Uint8Array(ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES + 5);
    for (let index = 0; index < bytes.length; index += 1) bytes[index] = (index * 17 + 9) & 0xff;
    const progress: number[] = [];

    const result = await sendAttachmentFile({
      connection,
      sessionId: "session-a",
      uploadId: "upload-a",
      file: new File([bytes], "direct.bin"),
      onProgress: (value) => progress.push(value),
      readCoordinatorStatus: async () => { throw new Error("status was not needed"); },
    });

    expect(socket.url).toBe("ws://127.0.0.1:4104/ws/local-attachment-transfer");
    expect(socket.protocol).toBe("roost-local-attachment-transfer-v1");
    expect(chunks.map((chunk) => ({ seq: chunk.seq, offset: chunk.offset, last: chunk.last }))).toEqual([
      { seq: 0, offset: 0n, last: false },
      { seq: 1, offset: BigInt(ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES), last: true },
    ]);
    expect(progress).toEqual([ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES, bytes.byteLength]);
    expect(result).toEqual({ abs_path: "/worker/direct.bin" });
    expect(socket.closed).toBe(true);
  });
  test("requests direct status on the authenticated control socket", async () => {
    const directGrant = grant();
    const sockets: FakeSocket[] = [];
    const opening = openAttachmentLoopbackTransfer(
      { door: { origin: "http://127.0.0.1:4104", workerFingerprint: "worker-a" }, grant: directGrant },
      { createSocket: (url, protocol) => {
        const socket = new FakeSocket(url, protocol);
        sockets.push(socket);
        return socket as unknown as WebSocket;
      } },
    );
    const socket = sockets.at(0);
    if (!socket) throw new Error("loopback socket was not created");
    socket.onClientFrame = (frame) => {
      if (frame.case === "hello") {
        socket.deliver({
          case: "ready",
          value: create(AttachmentTransferReadySchema, {
            workerFingerprint: "worker-a",
            workerEpoch: "epoch-a",
            sessionId: "session-a",
            uploadId: "upload-a",
          }),
        });
      }
      if (frame.case === "statusRequest") {
        socket.deliver({
          case: "status",
          value: create(AttachmentTransferStatusSchema, {
            uploadId: "upload-a",
            nextSeq: 0,
            bytesReceived: 0n,
            lastChunkSha256: "",
            committed: false,
            absPath: "",
            error: "",
          }),
        });
      }
    };
    socket.open();
    const connection = await opening;

    await expect(connection.requestStatus("upload-a")).resolves.toEqual({
      uploadId: "upload-a",
      nextSeq: 0,
      bytesReceived: 0,
      lastChunkSha256: "",
      committed: false,
      absPath: "",
      error: "",
    });
    connection.close("test complete");
  });
});
