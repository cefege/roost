// Attachment smoke checks must exercise the same chunked paths used by real browser uploads.
// These probes create deterministic bytes, query worker deduplication, and verify downloads.
// The smoke backdoor delegates here so file-transfer diagnostics stay separate from terminal DOM reads.
// Dynamic attachment loading preserves the smoke bundle's existing on-demand upload boundary.

import { coordClient } from "../connect.ts";
import type { SmokeApi } from "./smokeTypes.ts";

type SmokeFileTransferMethods = Pick<
  SmokeApi,
  "uploadAttachment" | "attachmentProbe" | "downloadWorkerFile"
>;

export function createSmokeFileTransferMethods(): SmokeFileTransferMethods {
  return {
    async uploadAttachment(sessionId, sizeBytes, filename = `smoke-${sizeBytes}.bin`) {
      const { uploadAttachment } = await import("./attachments.ts");
      // Distinct bytes ensure corrupt chunk assembly changes the integrity digest.
      const bytes = new Uint8Array(sizeBytes);
      for (let index = 0; index < sizeBytes; index++) bytes[index] = index & 0xff;
      const file = new File([bytes], filename, { type: "application/octet-stream" });
      return uploadAttachment({ id: sessionId }, file);
    },
    async attachmentProbe(sessionId, sha256, sizeBytes, filename = "probe.bin") {
      const response = await coordClient.attachmentProbe({
        sessionId,
        sha256,
        size: BigInt(sizeBytes),
        filename,
        shortPath: false,
      });
      return { hit: response.hit, abs_path: response.absPath };
    },
    async downloadWorkerFile(workerFp, path) {
      const chunkSize = 4 * 1024 * 1024;
      const parts: Uint8Array[] = [];
      let offset = 0;
      let byteCount = 0;
      for (;;) {
        const response = await coordClient.filesReadChunk({
          workerFp,
          path,
          offset: BigInt(offset),
          len: chunkSize,
        });
        if (response.data.length > 0) {
          parts.push(response.data);
          offset += response.data.length;
          byteCount += response.data.length;
        }
        if (response.eof || response.data.length === 0) break;
      }
      const completeFile = new Uint8Array(byteCount);
      let writeOffset = 0;
      for (const part of parts) {
        completeFile.set(part, writeOffset);
        writeOffset += part.length;
      }
      const digest = await crypto.subtle.digest("SHA-256", completeFile);
      const sha256 = [...new Uint8Array(digest)]
        .map((byte) => byte.toString(16).padStart(2, "0"))
        .join("");
      return { bytes: byteCount, sha256 };
    },
  };
}
