// Pins the streaming gzip contract backup.ts depends on: a multi-chunk file
// round-trips byte-for-byte through Bun.gunzipSync, the archive is created 0600,
// and a failed run leaves no partial archive behind.
import { afterEach, describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { GZIP_FILE_CHUNK_BYTES, gzipFileToPath } from "../src/gzip-file.ts";

const workdirs: string[] = [];

afterEach(() => {
  for (const dir of workdirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

function makeWorkdir(): string {
  const dir = mkdtempSync(join(tmpdir(), "roost-gzip-file-"));
  workdirs.push(dir);
  return dir;
}

/** Compressible body (a repeating 4 KiB pattern) with noise so it is not trivial. */
function makeSourceBytes(size: number): Uint8Array {
  const pattern = new Uint8Array(4096);
  for (let idx = 0; idx < pattern.length; idx += 1) pattern[idx] = idx % 251;
  const bytes = new Uint8Array(size);
  for (let offset = 0; offset < size; offset += pattern.length) {
    bytes.set(pattern.subarray(0, Math.min(pattern.length, size - offset)), offset);
  }
  const noise = new Uint8Array(1024);
  crypto.getRandomValues(noise);
  bytes.set(noise, Math.floor(size / 2));
  return bytes;
}

describe("gzipFileToPath", () => {
  test("a multi-chunk file round-trips byte-for-byte through gunzip", async () => {
    const dir = makeWorkdir();
    const sourcePath = join(dir, "source.bin");
    const destPath = join(dir, "source.bin.gz");
    const sourceBytes = makeSourceBytes(GZIP_FILE_CHUNK_BYTES * 3 + 1_234);
    writeFileSync(sourcePath, sourceBytes);

    const { bytesIn, bytesOut } = await gzipFileToPath(sourcePath, destPath);

    expect(bytesIn).toBe(statSync(sourcePath).size);
    expect(bytesOut).toBe(statSync(destPath).size);
    expect(bytesOut).toBeLessThan(bytesIn);

    const archive = new Uint8Array(await Bun.file(destPath).arrayBuffer());
    expect(Buffer.compare(Bun.gunzipSync(archive), sourceBytes)).toBe(0);
  });

  test("the archive is created owner-only", async () => {
    const dir = makeWorkdir();
    const sourcePath = join(dir, "source.bin");
    const destPath = join(dir, "source.bin.gz");
    writeFileSync(sourcePath, makeSourceBytes(4096));

    await gzipFileToPath(sourcePath, destPath);

    expect(statSync(destPath).mode & 0o777).toBe(0o600);
  });

  test("a missing source rejects and leaves no archive behind", async () => {
    const dir = makeWorkdir();
    const destPath = join(dir, "absent.bin.gz");

    await expect(gzipFileToPath(join(dir, "absent.bin"), destPath)).rejects.toThrow();

    expect(existsSync(destPath)).toBe(false);
  });

  // /dev/full accepts the open and fails every write with ENOSPC, which is the
  // small-host failure this guards: the drain pump dies, nothing pulls the
  // compressor's readable side, and an unraced writer.ready would hang the
  // nightly backup forever instead of reporting a failed run. The dest is a
  // SYMLINK to the device, never the device path: the failure path unlinks its
  // partial archive, and as root that would delete /dev/full itself.
  test.skipIf(process.platform !== "linux")(
    "a sink that fails mid-stream rejects instead of hanging",
    async () => {
      const dir = makeWorkdir();
      const sourcePath = join(dir, "source.bin");
      const destPath = join(dir, "full.gz");
      symlinkSync("/dev/full", destPath);
      // Incompressible, so the compressor emits output while the feed loop runs.
      const sourceBytes = new Uint8Array(GZIP_FILE_CHUNK_BYTES * 3);
      for (let offset = 0; offset < sourceBytes.length; offset += 65_536) {
        crypto.getRandomValues(sourceBytes.subarray(offset, offset + 65_536));
      }
      writeFileSync(sourcePath, sourceBytes);

      await expect(gzipFileToPath(sourcePath, destPath)).rejects.toThrow();
      expect(existsSync("/dev/full")).toBe(true);
    },
    10_000,
  );
});
