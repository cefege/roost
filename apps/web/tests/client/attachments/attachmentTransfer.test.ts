// Direct attachment transfer coverage for exact chunks, receipts, and ACK-loss recovery.
// Fakes model one authenticated carrier without standing up a worker or coordinator.
// The observable contract is byte order, progress, final path, and pinned status settlement.

import { describe, expect, mock, test } from "bun:test";
import {
  ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES,
  isAttachmentTransferChunkSha256,
} from "@roost/protocol/attachment-transfer";
import {
  AttachmentTransferCarrierError,
  sendAttachmentFile,
  type AttachmentFileTransferOptions,
  type AttachmentTransferAck,
  type AttachmentTransferChunk,
  type AttachmentTransferConnection,
  type AttachmentTransferStatus,
} from "../../../src/client/attachments/attachmentTransfer.ts";

class RecordingConnection implements AttachmentTransferConnection {
  sentChunk = false;
  closedReason = "";
  readonly chunks: AttachmentTransferChunk[] = [];

  async sendChunk(chunk: AttachmentTransferChunk): Promise<AttachmentTransferAck> {
    this.sentChunk = true;
    this.chunks.push(chunk);
    return {
      bytesReceived: Number(chunk.offset) + chunk.data.byteLength,
      absPath: chunk.last ? "/worker/received.bin" : "",
      chunkSha256: chunk.chunkSha256,
    };
  }

  requestStatus(): Promise<AttachmentTransferStatus> {
    return Promise.reject(new Error("status was not needed"));
  }

  close(reason: string): void {
    this.closedReason = reason;
  }
}

function transferOptions(
  connection: AttachmentTransferConnection,
  file: File,
  readCoordinatorStatus: AttachmentFileTransferOptions["readCoordinatorStatus"] = async () => {
    throw new Error("coordinator status was not needed");
  },
): AttachmentFileTransferOptions {
  return {
    connection,
    sessionId: "session-a",
    uploadId: "upload-a",
    file,
    readCoordinatorStatus,
  };
}

describe("sendAttachmentFile", () => {
  test("sends ordered 512 KiB byte slices and advances only from matching ACKs", async () => {
    const bytes = new Uint8Array(ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES + 3);
    for (let index = 0; index < bytes.length; index += 1) bytes[index] = (index * 13 + 5) & 0xff;
    const connection = new RecordingConnection();
    const progress: number[] = [];

    const result = await sendAttachmentFile({
      ...transferOptions(connection, new File([bytes], "received.bin")),
      onProgress: (received) => progress.push(received),
    });

    expect(result).toEqual({ abs_path: "/worker/received.bin" });
    expect(connection.chunks.map((chunk) => ({ seq: chunk.seq, offset: chunk.offset, last: chunk.last }))).toEqual([
      { seq: 0, offset: 0n, last: false },
      { seq: 1, offset: BigInt(ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES), last: true },
    ]);
    expect(connection.chunks.every((chunk) => isAttachmentTransferChunkSha256(chunk.chunkSha256))).toBe(true);
    expect(progress).toEqual([ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES, bytes.byteLength]);
    const received = new Uint8Array(connection.chunks.reduce((total, chunk) => total + chunk.data.byteLength, 0));
    let offset = 0;
    for (const chunk of connection.chunks) {
      received.set(chunk.data, offset);
      offset += chunk.data.byteLength;
    }
    expect(received).toEqual(bytes);
    expect(connection.closedReason).toBe("complete");
  });

  test("preserves a zero-byte file as one final direct chunk", async () => {
    const connection = new RecordingConnection();
    const progress: number[] = [];

    const result = await sendAttachmentFile({
      ...transferOptions(connection, new File([], "empty.bin")),
      onProgress: (received) => progress.push(received),
    });

    expect(result).toEqual({ abs_path: "/worker/received.bin" });
    expect(connection.chunks).toHaveLength(1);
    expect(connection.chunks[0]).toMatchObject({ seq: 0, offset: 0n, last: true });
    expect(connection.chunks[0]?.data).toEqual(new Uint8Array());
    expect(progress).toEqual([0]);
  });

  test("settles a lost final ACK from its authenticated direct receipt", async () => {
    let pending: AttachmentTransferChunk | null = null;
    const directStatus = mock<AttachmentTransferConnection["requestStatus"]>(async (uploadId) => {
      const pendingChunk = pending;
      if (!pendingChunk) throw new Error("missing pending chunk");
      if (uploadId !== pendingChunk.uploadId) throw new Error("unexpected upload id");
      return {
        uploadId: pendingChunk.uploadId,
        nextSeq: pendingChunk.seq + 1,
        bytesReceived: Number(pendingChunk.offset) + pendingChunk.data.byteLength,
        lastChunkSha256: pendingChunk.chunkSha256,
        committed: true,
        absPath: "/worker/final-receipt.bin",
        error: "",
      };
    });
    const connection: AttachmentTransferConnection = {
      sentChunk: true,
      async sendChunk(chunk) {
        pending = chunk;
        throw new AttachmentTransferCarrierError("lost acknowledgement", true, true);
      },
      requestStatus: directStatus,
      close: () => undefined,
    };
    const coordinatorStatus = mock<AttachmentFileTransferOptions["readCoordinatorStatus"]>(async () => {
      throw new Error("coordinator status should not run");
    });

    const result = await sendAttachmentFile({
      ...transferOptions(connection, new File([new Uint8Array([7, 8])], "final.bin"), coordinatorStatus),
    });

    expect(result).toEqual({ abs_path: "/worker/final-receipt.bin" });
    expect(directStatus).toHaveBeenCalledTimes(1);
    expect(coordinatorStatus).not.toHaveBeenCalled();
  });

  test("uses coordinator status only to settle a lost final receipt after the carrier dies", async () => {
    let pending: AttachmentTransferChunk | null = null;
    const connection: AttachmentTransferConnection = {
      sentChunk: true,
      async sendChunk(chunk) {
        pending = chunk;
        throw new AttachmentTransferCarrierError("carrier closed", true, true);
      },
      requestStatus: async () => { throw new Error("carrier is closed"); },
      close: () => undefined,
    };
    const coordinatorStatus = mock<AttachmentFileTransferOptions["readCoordinatorStatus"]>(async () => {
      const pendingChunk = pending;
      if (!pendingChunk) throw new Error("missing pending chunk");
      return {
        uploadId: pendingChunk.uploadId,
        nextSeq: 1,
        bytesReceived: pendingChunk.data.byteLength,
        lastChunkSha256: pendingChunk.chunkSha256,
        committed: true,
        absPath: "/worker/durable-final.bin",
        error: "",
      };
    });

    await expect(sendAttachmentFile({
      ...transferOptions(connection, new File([new Uint8Array([1])], "durable.bin"), coordinatorStatus),
    })).resolves.toEqual({ abs_path: "/worker/durable-final.bin" });
    expect(coordinatorStatus).toHaveBeenCalledWith("session-a", "upload-a");
  });
  test("does not resume a nonfinal direct upload through coordinator status", async () => {
    let firstChunk: AttachmentTransferChunk | null = null;
    const sentSeq: number[] = [];
    const connection: AttachmentTransferConnection = {
      sentChunk: true,
      async sendChunk(chunk) {
        sentSeq.push(chunk.seq);
        if (chunk.seq === 0) {
          firstChunk = chunk;
          throw new AttachmentTransferCarrierError("lost first acknowledgement", true, true);
        }
        throw new AttachmentTransferCarrierError("direct route is gone", true);
      },
      requestStatus: async () => { throw new Error("direct route is gone"); },
      close: () => undefined,
    };
    const coordinatorStatus = mock<AttachmentFileTransferOptions["readCoordinatorStatus"]>(async () => {
      const pendingChunk = firstChunk;
      if (!pendingChunk) throw new Error("missing first chunk");
      return {
        uploadId: pendingChunk.uploadId,
        nextSeq: 1,
        bytesReceived: pendingChunk.data.byteLength,
        lastChunkSha256: pendingChunk.chunkSha256,
        committed: false,
        absPath: "",
        error: "",
      };
    });

    await expect(sendAttachmentFile({
      ...transferOptions(
        connection,
        new File([new Uint8Array(ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES + 1)], "nonfinal.bin"),
        coordinatorStatus,
      ),
    })).rejects.toThrow("direct route is gone");
    expect(sentSeq).toEqual([0, 1]);
    expect(coordinatorStatus).toHaveBeenCalledTimes(1);
  });
});
