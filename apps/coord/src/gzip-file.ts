// Streaming gzip of one file into another, in fixed 1 MiB slices, so peak heap
// tracks the chunk size instead of the input size. backup.ts calls this for the
// nightly archive; node:zlib is banned in apps/coord (heap-corruption segfault
// under Bun, enforced by scripts/lint-roost.ts), so the compressor is the global
// CompressionStream. Callers own the state-transition log line.

import { closeSync, openSync, readSync, rmSync, writeSync } from "node:fs";

export const GZIP_FILE_CHUNK_BYTES = 1024 * 1024;

export interface GzipFileResult {
  bytesIn: number;
  bytesOut: number;
}

/** Compress sourcePath into a fresh 0600 gzip archive at destPath. */
export async function gzipFileToPath(sourcePath: string, destPath: string): Promise<GzipFileResult> {
  const compressor = new CompressionStream("gzip");
  const writer = compressor.writable.getWriter();
  const reader = compressor.readable.getReader();

  const srcFd = openSync(sourcePath, "r");
  let destFd: number;
  try {
    destFd = openSync(destPath, "w", 0o600);
  } catch (error) {
    closeSync(srcFd);
    throw error;
  }

  let bytesIn = 0;
  let bytesOut = 0;

  // The drain pump starts before any input is written: a CompressionStream whose
  // readable side is never pulled stalls the writer once its queue fills. Every
  // await below races it, because a pump that dies (ENOSPC on the archive) stops
  // draining, and an unraced `writer.ready` would then never settle — a silent
  // hang holding the snapshot open instead of a failed backup.
  const pump = (async () => {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      writeSync(destFd, value);
      bytesOut += value.byteLength;
    }
  })();

  try {
    const buffer = new Uint8Array(GZIP_FILE_CHUNK_BYTES);
    for (;;) {
      const bytesRead = readSync(srcFd, buffer, 0, GZIP_FILE_CHUNK_BYTES, null);
      if (bytesRead === 0) break;
      bytesIn += bytesRead;
      // Racing writer.ready is what bounds memory: without the await the writable
      // queue grows to the whole file (measured 391 MB on a 328 MB input).
      const ready = writer.ready;
      ready.catch(() => {});
      await Promise.race([ready, pump]);
      // .slice() is load-bearing: the writer takes the chunk asynchronously, so
      // handing it a view of the reused buffer corrupts the archive.
      const written = writer.write(buffer.subarray(0, bytesRead).slice());
      written.catch(() => {});
      await Promise.race([written, pump]);
    }
    const closed = writer.close();
    closed.catch(() => {});
    await Promise.race([closed, pump]);
    await pump;
    return { bytesIn, bytesOut };
  } catch (error) {
    // The pump must stop before the descriptors close, or its next writeSync
    // lands on a closed fd as an unhandled rejection. Cancel the READABLE side
    // to settle it: `writer.abort()` never settles while a reader still holds
    // the readable side with compressed bytes buffered (Bun 1.3.14), which is
    // itself a hang, and it is reached on exactly the sink failure it was
    // supposed to clean up after.
    await reader.cancel().catch(() => {});
    await pump.catch(() => {});
    // A partial .gz must never survive for a later reader to trust. An
    // unlinkable dest must not mask the failure that brought us here.
    try { rmSync(destPath, { force: true }); } catch { /* report the original error */ }
    throw error;
  } finally {
    closeSync(srcFd);
    closeSync(destFd);
  }
}
