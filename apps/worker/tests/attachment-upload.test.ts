// att1-stream — worker-side chunked-upload assembler. Drives
// handleAttachmentChunk directly: multi-chunk byte-fidelity, empty file,
// short-path symlink, and the path-traversal guard. Writes under a unique
// throwaway session id in the real attachment base dir; cleans up after.

import { test, expect, afterAll } from "bun:test";
import fs from "node:fs";
import { handleAttachmentChunk, probeAttachment, recordAttachmentHash } from "../src/attachment-upload.ts";
import { attachmentSessionDir } from "../src/attachment-reaper.ts";

const SID = `test-upload-${crypto.randomUUID()}`;
const dir = attachmentSessionDir(SID);

afterAll(() => { try { fs.rmSync(dir, { recursive: true, force: true }); } catch { /* ignore */ } });

function feed(reqId: string, filename: string, slices: Uint8Array[], shortPath = false): Promise<string> {
  return new Promise((resolve, reject) => {
    const reply = { ok: resolve, err: reject };
    slices.forEach((data, i) => {
      handleAttachmentChunk({
        request_id: reqId, session_id: SID, filename, short_path: shortPath,
        data, last: i === slices.length - 1, seq: i,
      }, reply);
    });
  });
}

test("multi-chunk assembles byte-exact in order", async () => {
  // 2.5 MiB across 1 MiB chunks → distinct bytes per chunk so reorder shows.
  const total = new Uint8Array(2_500_000).map((_, i) => i & 0xff);
  const CHUNK = 1024 * 1024;
  const slices: Uint8Array[] = [];
  for (let o = 0; o < total.length; o += CHUNK) slices.push(total.subarray(o, o + CHUNK));
  const absPath = await feed(crypto.randomUUID(), "blob.bin", slices);
  const written = new Uint8Array(fs.readFileSync(absPath));
  expect(written.length).toBe(total.length);
  expect(Buffer.from(written).equals(Buffer.from(total))).toBe(true);
});

test("original filename preserved; duplicates get ` (n)` suffix", async () => {
  const a = await feed(crypto.randomUUID(), "dup.txt", [new TextEncoder().encode("first")]);
  const b = await feed(crypto.randomUUID(), "dup.txt", [new TextEncoder().encode("second")]);
  const c = await feed(crypto.randomUUID(), "dup.txt", [new TextEncoder().encode("third")]);
  expect(a.endsWith("/dup.txt")).toBe(true);          // exact original name
  expect(b.endsWith("/dup (2).txt")).toBe(true);
  expect(c.endsWith("/dup (3).txt")).toBe(true);
  expect(fs.readFileSync(a, "utf8")).toBe("first");    // no clobber
  expect(fs.readFileSync(b, "utf8")).toBe("second");
});

test("empty file (single empty last chunk) creates a 0-byte file", async () => {
  const absPath = await feed(crypto.randomUUID(), "empty.txt", [new Uint8Array(0)]);
  expect(fs.statSync(absPath).size).toBe(0);
});

test("short_path returns a .shortcuts/pN symlink to the real file", async () => {
  const data = new TextEncoder().encode("hello");
  const absPath = await feed(crypto.randomUUID(), "note.txt", [data], true);
  expect(absPath).toContain("/.shortcuts/p");
  expect(fs.readFileSync(absPath, "utf8")).toBe("hello");  // resolves through symlink
});

test("path-traversal session_id is rejected, no file written", async () => {
  const err = await new Promise<string>((resolve) => {
    handleAttachmentChunk(
      { request_id: crypto.randomUUID(), session_id: "../../etc", filename: "x", short_path: false, data: new Uint8Array([1]), last: true, seq: 0 },
      { ok: () => resolve("UNEXPECTED_OK"), err: resolve },
    );
  });
  expect(err).toBe("invalid session_id");
});

// Silent-truncation guard: a chunk arriving with seq>0 but no in-flight state
// (its prior chunk errored / it was idle-reaped) must be REFUSED, not treated
// as a fresh single-chunk upload and renamed as a truncated file.
test("continuation chunk (seq>0) with no in-flight state is refused", async () => {
  const err = await new Promise<string>((resolve) => {
    handleAttachmentChunk(
      { request_id: crypto.randomUUID(), session_id: SID, filename: "ghost.bin", short_path: false, data: new Uint8Array([9, 9, 9]), last: true, seq: 1 },
      { ok: () => resolve("UNEXPECTED_OK"), err: resolve },
    );
  });
  expect(err).toContain("not in progress");
  expect(fs.existsSync(`${dir}/ghost.bin`)).toBe(false);
});

test("out-of-order chunk aborts the upload (no file, temp cleaned)", async () => {
  const id = crypto.randomUUID();
  handleAttachmentChunk(  // seq 0 opens the temp file
    { request_id: id, session_id: SID, filename: "ooo.bin", short_path: false, data: new Uint8Array([1, 2, 3]), last: false, seq: 0 },
    { ok: () => {}, err: () => {} },
  );
  const err = await new Promise<string>((resolve) => {  // seq 2 skips 1 → abort
    handleAttachmentChunk(
      { request_id: id, session_id: SID, filename: "ooo.bin", short_path: false, data: new Uint8Array([4, 5, 6]), last: true, seq: 2 },
      { ok: () => resolve("UNEXPECTED_OK"), err: resolve },
    );
  });
  expect(err).toContain("out of order");
  expect(fs.existsSync(`${dir}/ooo.bin`)).toBe(false);
  expect(fs.existsSync(`${dir}/.upload-${id}`)).toBe(false);  // temp reaped
});

// ─── att3: content-dedup manifest ───────────────────────────────────────────

test("upload records its content hash; probe hits with the saved path", async () => {
  const bytes = new Uint8Array(300_000).map((_, i) => (i * 7) & 0xff);
  const CHUNK = 1024 * 1024;
  const slices: Uint8Array[] = [];
  for (let o = 0; o < bytes.length; o += CHUNK) slices.push(bytes.subarray(o, o + CHUNK));
  const absPath = await feed(crypto.randomUUID(), "dedup.bin", slices);
  const sha = new Bun.CryptoHasher("sha256").update(bytes).digest("hex");
  expect(probeAttachment(SID, sha, false)).toEqual({ hit: true, abs_path: absPath });
});

test("probe misses for content that was never uploaded", () => {
  expect(probeAttachment(SID, "0".repeat(64), false)).toEqual({ hit: false, abs_path: "" });
});

test("probe misses after the recorded file is unlinked", async () => {
  const bytes = new TextEncoder().encode("ephemeral");
  const absPath = await feed(crypto.randomUUID(), "gone.txt", [bytes]);
  const sha = new Bun.CryptoHasher("sha256").update(bytes).digest("hex");
  expect(probeAttachment(SID, sha, false).hit).toBe(true);
  fs.unlinkSync(absPath);
  expect(probeAttachment(SID, sha, false)).toEqual({ hit: false, abs_path: "" });
});

test("re-recording a name with new content prunes the stale hash", () => {
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(`${dir}/over.txt`, "v2");
  const hashV1 = "a".repeat(64);
  const hashV2 = "b".repeat(64);
  recordAttachmentHash(dir, hashV1, "over.txt");
  recordAttachmentHash(dir, hashV2, "over.txt");  // same name, new content → prune v1
  expect(probeAttachment(SID, hashV1, false).hit).toBe(false);  // stale key gone
  expect(probeAttachment(SID, hashV2, false)).toEqual({ hit: true, abs_path: `${dir}/over.txt` });
});
