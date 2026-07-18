// read-file + list-dir RPCs invoked by CoordLink.onBrowserCommand for
// the matching ClientControlFrame variants. Reply is rpc-ok or rpc-error
// upstream on CoordLink; coord forwards via pending-rpcs back to the
// originating browser.

import { stat, readFile, readdir, mkdir, open } from "node:fs/promises";
import { join } from "node:path";
import { expandTilde } from "./util/path.ts";

// ponytail: 25 MB unary ceiling covers source/logs/images the user downloads
// via a terminal file link. Bigger → chunk it (see AttachFileChunk for the
// reverse direction). Base64 in one response = ~1.33× in RAM, fine occasionally.
const READ_FILE_MAX_BYTES = 25 * 1024 * 1024;
const LIST_DIR_MAX_ENTRIES = 200;

export type RpcReply =
  | { kind: "rpc-ok"; data: unknown }
  | { kind: "rpc-error"; message: string };

export async function readFileRpc(path: string): Promise<RpcReply> {
  try {
    path = expandTilde(path);
    const s = await stat(path);
    if (s.size > READ_FILE_MAX_BYTES) {
      return { kind: "rpc-error", message: `read-file: file too large (${s.size} bytes, max ${READ_FILE_MAX_BYTES})` };
    }
    const buf = await readFile(path);
    return { kind: "rpc-ok", data: { content_b64: buf.toString("base64"), size: buf.length } };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return { kind: "rpc-error", message: `read-file: ${msg}` };
  }
}

// Chunked byte-range read backing the SPA's progress-tracking download.
// No size ceiling (bounded per-chunk); the browser drives the offset loop
// and stops on eof. size is the file's total so the SPA can show a bar.
export async function readFileChunkRpc(path: string, offset: number, len: number): Promise<RpcReply> {
  try {
    path = expandTilde(path);
    const s = await stat(path);
    const fh = await open(path, "r");
    try {
      const want = Math.min(len, Math.max(0, s.size - offset));
      const buf = Buffer.alloc(want);
      const { bytesRead } = await fh.read(buf, 0, want, offset);
      const eof = offset + bytesRead >= s.size;
      return { kind: "rpc-ok", data: { content_b64: buf.subarray(0, bytesRead).toString("base64"), size: s.size, eof } };
    } finally {
      await fh.close();
    }
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return { kind: "rpc-error", message: `read-file-chunk: ${msg}` };
  }
}

export async function mkdirRpc(path: string): Promise<RpcReply> {
  try {
    const resolved = expandTilde(path);
    // recursive so a typed deep path (~/Code/new/sub) creates intermediates;
    // no-op if it already exists.
    await mkdir(resolved, { recursive: true });
    return { kind: "rpc-ok", data: { resolved_path: resolved } };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return { kind: "rpc-error", message: `mkdir: ${msg}` };
  }
}

export async function listDirRpc(path: string): Promise<RpcReply> {
  try {
    const resolved = expandTilde(path);
    const dirents = await readdir(resolved, { withFileTypes: true });
    // Dirs first (alphabetical), then files (alphabetical). The browse page
    // renders folders as drill-in tiles/rows and files as view-only entries
    // beneath them, so a stable dirs-before-files order keeps both scannable.
    const sorted = dirents
      .sort((a, b) => (a.isDirectory() === b.isDirectory()
        ? a.name.localeCompare(b.name)
        : a.isDirectory() ? -1 : 1))
      .slice(0, LIST_DIR_MAX_ENTRIES);
    // stat each entry for mtime ("Modified …" tooltip in the browse grid).
    // Best-effort: skip mtime on stat error rather than failing the whole list.
    const entries = await Promise.all(
      sorted.map(async (d) => {
        try {
          const s = await stat(join(resolved, d.name));
          return { name: d.name, isDir: d.isDirectory(), mtime_ms: Math.floor(s.mtimeMs) };
        } catch {
          return { name: d.name, isDir: d.isDirectory(), mtime_ms: 0 };
        }
      }),
    );
    return { kind: "rpc-ok", data: { entries, resolved_path: resolved } };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return { kind: "rpc-error", message: `list-dir: ${msg}` };
  }
}
