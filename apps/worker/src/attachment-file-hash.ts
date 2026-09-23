// Streaming SHA-256 for attachment temp and destination files.
// The operation owner rebuilds an incremental digest after restart; destination
// recovery verifies an occupied final name before it can change the manifest.

import fs from "node:fs";
import { ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES } from "@roost/shared/attachment-transfer";

export function hashAttachmentFile(filePath: string): Bun.CryptoHasher {
  const hasher = new Bun.CryptoHasher("sha256");
  const fd = fs.openSync(filePath, "r");
  const buffer = new Uint8Array(ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES);
  try {
    for (;;) {
      const read = fs.readSync(fd, buffer, 0, buffer.byteLength, null);
      if (read === 0) return hasher;
      hasher.update(buffer.subarray(0, read));
    }
  } finally {
    fs.closeSync(fd);
  }
}

export function sha256AttachmentFile(filePath: string): string {
  const hasher = hashAttachmentFile(filePath);
  return hasher.digest("hex");
}
