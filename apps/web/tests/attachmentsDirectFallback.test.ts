// Coordinator relay fallback coverage when direct attachment transport is unavailable.
// The direct selector is mocked to null so these assertions pin the unchanged relay contract.
// Public upload still preserves empty files, sequence ordering, and ACK-driven progress.

import { beforeEach, describe, expect, mock, test } from "bun:test";

const attachFileChunk = mock(async (request: {
  uploadId: string;
  sessionId: string;
  filename: string;
  shortPath: boolean;
  data: Uint8Array;
  last: boolean;
  seq: number;
}) => ({ absPath: request.last ? `/worker/${request.filename}` : "" }));

mock.module("../src/client/rpc/connect.ts", () => ({ coordClient: { attachFileChunk } }));
mock.module("../src/client/attachments/attachmentDirect.ts", () => ({ uploadAttachmentDirect: async () => null }));
mock.module("../src/lib/userTerminalInput.ts", () => ({ sendUserTerminalInput: () => undefined }));
mock.module("../src/store/transfers.ts", () => ({
  addTransfer: () => undefined,
  markTransferState: () => undefined,
  setTransferProgress: () => undefined,
}));

// Dynamic import is required so Bun installs the direct and coordinator mocks first.
const { uploadAttachment } = await import("../src/lib/attachments.ts");

describe("uploadAttachment coordinator fallback", () => {
  beforeEach(() => {
    attachFileChunk.mockClear();
  });

  test("retains existing relay chunk fields and ordered progress when direct is unavailable", async () => {
    const relayChunkBytes = 4 * 1024 * 1024;
    const bytes = new Uint8Array(relayChunkBytes + 1);
    for (let index = 0; index < bytes.length; index += 1) bytes[index] = (index * 7 + 3) & 0xff;
    const progress: number[] = [];

    const result = await uploadAttachment(
      { id: "session-a", worker_fp: "worker-a" },
      new File([bytes], "relay.bin"),
      (received) => progress.push(received),
    );

    expect(result).toEqual({ abs_path: "/worker/relay.bin" });
    expect(attachFileChunk.mock.calls.map(([chunk]) => ({
      sessionId: chunk.sessionId,
      filename: chunk.filename,
      shortPath: chunk.shortPath,
      seq: chunk.seq,
      last: chunk.last,
      bytes: chunk.data.byteLength,
    }))).toEqual([
      { sessionId: "session-a", filename: "relay.bin", shortPath: false, seq: 0, last: false, bytes: relayChunkBytes },
      { sessionId: "session-a", filename: "relay.bin", shortPath: false, seq: 1, last: true, bytes: 1 },
    ]);
    expect(progress).toEqual([relayChunkBytes, bytes.byteLength]);
  });

  test("keeps a zero-byte file as one final coordinator relay call", async () => {
    const progress: number[] = [];

    await expect(uploadAttachment(
      { id: "session-empty", worker_fp: "worker-a" },
      new File([], "empty.bin"),
      (received) => progress.push(received),
    )).resolves.toEqual({ abs_path: "/worker/empty.bin" });

    expect(attachFileChunk).toHaveBeenCalledTimes(1);
    expect(attachFileChunk.mock.calls[0]?.[0]).toMatchObject({
      sessionId: "session-empty",
      filename: "empty.bin",
      seq: 0,
      last: true,
      data: new Uint8Array(),
    });
    expect(progress).toEqual([0]);
  });
});
