// file-rpcs.test.ts — chunked byte-range read (readFileChunkRpc). This is where
// off-by-one bugs in a chunked reader live, so cover the boundary cases the
// gated e2e can't guarantee to run: exact-multiple, partial tail, empty file,
// and a read at/past EOF.

import { test, expect, afterAll } from "bun:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { readFileChunkRpc } from "../src/file-rpcs.ts";

const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "roost-readchunk-"));
afterAll(() => { try { fs.rmSync(tmpDir, { recursive: true, force: true }); } catch { /* ignore */ } });

function writeFile(name: string, bytes: Uint8Array): string {
  const p = path.join(tmpDir, name);
  fs.writeFileSync(p, bytes);
  return p;
}

interface Chunk { data: Uint8Array; size: number; eof: boolean }
async function readOne(p: string, offset: number, len: number): Promise<Chunk> {
  const r = await readFileChunkRpc(p, offset, len);
  if (r.kind !== "rpc-ok") throw new Error(r.message);
  const d = r.data as { content_b64: string; size: number; eof: boolean };
  return { data: new Uint8Array(Buffer.from(d.content_b64, "base64")), size: d.size, eof: d.eof };
}

async function readAll(p: string, chunk: number): Promise<{ bytes: Uint8Array; chunks: number; size: number }> {
  const parts: Uint8Array[] = [];
  let offset = 0;
  let chunks = 0;
  let size = 0;
  for (;;) {
    const c = await readOne(p, offset, chunk);
    size = c.size;
    chunks++;
    if (c.data.length) { parts.push(c.data); offset += c.data.length; }
    if (c.eof || c.data.length === 0) break;
  }
  const bytes = new Uint8Array(offset);
  let o = 0;
  for (const part of parts) { bytes.set(part, o); o += part.length; }
  return { bytes, chunks, size };
}

test("exact-multiple: reassembles byte-exact, eof on the final full chunk", async () => {
  const bytes = new Uint8Array(8).map((_, i) => i + 1);
  const p = writeFile("exact.bin", bytes);
  const got = await readAll(p, 4);
  expect(got.size).toBe(8);
  expect(got.chunks).toBe(2);                                  // 4 + 4, eof on the 2nd
  expect(Buffer.from(got.bytes).equals(Buffer.from(bytes))).toBe(true);
});

test("partial tail: last chunk is shorter and carries eof", async () => {
  const bytes = new Uint8Array(10).map((_, i) => (i * 3) & 0xff);
  const p = writeFile("tail.bin", bytes);
  const got = await readAll(p, 4);
  expect(got.size).toBe(10);
  expect(got.chunks).toBe(3);                                  // 4 + 4 + 2
  expect(Buffer.from(got.bytes).equals(Buffer.from(bytes))).toBe(true);
  const last = await readOne(p, 8, 4);
  expect(last.data.length).toBe(2);
  expect(last.eof).toBe(true);
});

test("empty file: one empty chunk with eof, size 0", async () => {
  const p = writeFile("empty.bin", new Uint8Array(0));
  const c = await readOne(p, 0, 4);
  expect(c.size).toBe(0);
  expect(c.data.length).toBe(0);
  expect(c.eof).toBe(true);
});

test("read at EOF returns empty + eof (no over-read)", async () => {
  const bytes = new Uint8Array(5).fill(7);
  const p = writeFile("ateof.bin", bytes);
  const c = await readOne(p, 5, 4);
  expect(c.data.length).toBe(0);
  expect(c.eof).toBe(true);
  expect(c.size).toBe(5);
});

test("oversized chunk request is capped below the worker WebSocket payload ceiling", async () => {
  const maxChunk = 2 * 1024 * 1024;
  const p = writeFile("oversized.bin", new Uint8Array(maxChunk + 1).fill(9));
  const c = await readOne(p, 0, Number.MAX_SAFE_INTEGER);
  expect(c.data.length).toBe(maxChunk);
  expect(c.eof).toBe(false);
});

test("missing file → rpc-error", async () => {
  const r = await readFileChunkRpc(path.join(tmpDir, "does-not-exist.bin"), 0, 4);
  expect(r.kind).toBe("rpc-error");
});
