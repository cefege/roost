// read-file + list-dir RPCs invoked by CoordLink.onBrowserCommand for
// the matching ClientControlFrame variants. Reply is rpc-ok or rpc-error
// upstream on CoordLink; coord forwards via pending-rpcs back to the
// originating browser.

import { stat, readFile, readdir, mkdir, open } from "node:fs/promises";
import { join } from "node:path";
import { supportedHostPlatform } from "@roost/shared/platform";
import {
  canonicalExistingWorkerPath,
  expandTilde,
  toFilesystemPath,
} from "./util/path.ts";

const HOST_PLATFORM = supportedHostPlatform();

function existingFilesystemPath(input: string): { display: string; filesystem: string } {
  const expanded = expandTilde(input, HOST_PLATFORM);
  const display = HOST_PLATFORM === "win32"
    ? canonicalExistingWorkerPath(expanded, HOST_PLATFORM)
    : expanded;
  return {
    display,
    filesystem: toFilesystemPath(display, HOST_PLATFORM),
  };
}

// ponytail: 25 MB unary ceiling covers source/logs/images the user downloads
// via a terminal file link. Bigger → chunk it (see AttachFileChunk for the
// reverse direction). Base64 in one response = ~1.33× in RAM, fine occasionally.
const READ_FILE_MAX_BYTES = 25 * 1024 * 1024;
const READ_FILE_CHUNK_MAX_BYTES = 2 * 1024 * 1024;
// RPC replies are JSON/base64 inside CoordWorkerUp. Keep encoded payload plus
// protobuf framing below coord's 4 MiB worker-WebSocket admission ceiling.
const LIST_DIR_MAX_ENTRIES = 200;

export type RpcReply =
  | { kind: "rpc-ok"; data: unknown }
  | { kind: "rpc-error"; message: string };

export async function readFileRpc(path: string): Promise<RpcReply> {
  try {
    const resolved = existingFilesystemPath(path);
    const s = await stat(resolved.filesystem);
    if (s.size > READ_FILE_MAX_BYTES) {
      return { kind: "rpc-error", message: `read-file: file too large (${s.size} bytes, max ${READ_FILE_MAX_BYTES})` };
    }
    const buf = await readFile(resolved.filesystem);
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
    const resolved = existingFilesystemPath(path);
    const s = await stat(resolved.filesystem);
    const fh = await open(resolved.filesystem, "r");
    try {
      const want = Math.min(Math.max(0, len), READ_FILE_CHUNK_MAX_BYTES, Math.max(0, s.size - offset));
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
    const expanded = expandTilde(path, HOST_PLATFORM);
    // recursive so a typed deep path creates intermediates; no-op if present.
    await mkdir(toFilesystemPath(expanded, HOST_PLATFORM), { recursive: true });
    const resolved = HOST_PLATFORM === "win32"
      ? canonicalExistingWorkerPath(expanded, HOST_PLATFORM)
      : expanded;
    return { kind: "rpc-ok", data: { resolved_path: resolved } };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return { kind: "rpc-error", message: `mkdir: ${msg}` };
  }
}

export async function listDirRpc(path: string): Promise<RpcReply> {
  try {
    const resolved = existingFilesystemPath(path);
    const dirents = await readdir(resolved.filesystem, { withFileTypes: true });
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
          const s = await stat(join(resolved.filesystem, d.name));
          return { name: d.name, isDir: d.isDirectory(), mtime_ms: Math.floor(s.mtimeMs) };
        } catch {
          return { name: d.name, isDir: d.isDirectory(), mtime_ms: 0 };
        }
      }),
    );
    return { kind: "rpc-ok", data: { entries, resolved_path: resolved.display } };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return { kind: "rpc-error", message: `list-dir: ${msg}` };
  }
}
