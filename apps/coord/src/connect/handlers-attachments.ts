// Worker-forwarding RPCs: file read/list-dir + attachment save/list/delete.
// Each resolves the session's worker hub socket, forwards a browser-command
// frame via sendBrowserCmd, and awaits the worker reply through
// createPendingRpc. Spread into router.ts's single router.service() literal.
// Split out of router.ts (400-line cap).

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  CoordinatorService,
  FilesReadResponseSchema, FilesReadChunkResponseSchema, FilesListDirResponseSchema, FilesListDirEntrySchema,
  FilesMkdirResponseSchema,
  AttachFileChunkResponseSchema, AttachmentProbeResponseSchema,
  ListAttachmentsResponseSchema, AttachmentEntrySchema,
  DeleteAttachmentResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { asSessionId } from "@roost/shared/wire";
import { requireAuth } from "./auth-interceptor.ts";
import { getWorkerHubSocket, sendAttachmentChunk } from "./worker-service.ts";
import { createPendingRpc, createPendingRpcWithId } from "../router/pending-rpcs.ts";
import { sendBrowserCmd, requireSessionWorkerSocket } from "./router-helpers.ts";
import type { ConnectDeps } from "./router.ts";

type AttachmentMethods =
  | "filesRead" | "filesReadChunk" | "filesListDir" | "filesMkdir"
  | "attachFileChunk" | "attachmentProbe" | "listAttachments" | "deleteAttachment";

export function makeAttachmentHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, AttachmentMethods> {
  return {
    async filesRead(req, ctx) {
      const caller = requireAuth(ctx.values);
      const sock = getWorkerHubSocket(req.workerFp);
      if (!sock) throw new ConnectError("worker not connected", Code.FailedPrecondition);
      const pending = createPendingRpc<{ content_b64: string; size: number }>(10_000, req.workerFp);
      sendBrowserCmd(sock, caller, pending.request_id, {
        kind: "read-file" as const, request_id: pending.request_id, path: req.path,
      });
      const data = await pending.promise;
      const bytes = data.content_b64
        ? Uint8Array.from(atob(data.content_b64), c => c.charCodeAt(0))
        : new Uint8Array(0);
      return create(FilesReadResponseSchema, { data: bytes, size: BigInt(data.size) });
    },

    async filesReadChunk(req, ctx) {
      const caller = requireAuth(ctx.values);
      const sock = getWorkerHubSocket(req.workerFp);
      if (!sock) throw new ConnectError("worker not connected", Code.FailedPrecondition);
      if (req.len <= 0 || req.len > 4 * 1024 * 1024) {
        throw new ConnectError("file chunk length must be between 1 and 4194304 bytes", Code.InvalidArgument);
      }
      const pending = createPendingRpc<{ content_b64: string; size: number; eof: boolean }>(30_000, req.workerFp);
      sendBrowserCmd(sock, caller, pending.request_id, {
        kind: "read-file-chunk" as const, request_id: pending.request_id, path: req.path, offset: Number(req.offset), len: req.len,
      });
      const data = await pending.promise;
      const bytes = data.content_b64
        ? Uint8Array.from(atob(data.content_b64), c => c.charCodeAt(0))
        : new Uint8Array(0);
      return create(FilesReadChunkResponseSchema, { data: bytes, size: BigInt(data.size), eof: data.eof });
    },

    async filesListDir(req, ctx) {
      const caller = requireAuth(ctx.values);
      const sock = getWorkerHubSocket(req.workerFp);
      if (!sock) throw new ConnectError("worker not connected", Code.FailedPrecondition);
      const pending = createPendingRpc<{ entries: Array<{ name: string; isDir: boolean; mtime_ms?: number }>; resolved_path?: string }>(10_000, req.workerFp);
      sendBrowserCmd(sock, caller, pending.request_id, {
        kind: "list-dir" as const, request_id: pending.request_id, path: req.path,
      });
      const data = await pending.promise;
      return create(FilesListDirResponseSchema, {
        entries: data.entries.map(e => create(FilesListDirEntrySchema, { name: e.name, isDir: e.isDir, mtimeMs: e.mtime_ms ? BigInt(e.mtime_ms) : 0n })),
        resolvedPath: data.resolved_path ?? req.path,
      });
    },

    async filesMkdir(req, ctx) {
      const caller = requireAuth(ctx.values);
      const sock = getWorkerHubSocket(req.workerFp);
      if (!sock) throw new ConnectError("worker not connected", Code.FailedPrecondition);
      const pending = createPendingRpc<{ resolved_path?: string }>(10_000, req.workerFp);
      sendBrowserCmd(sock, caller, pending.request_id, {
        kind: "mkdir" as const, request_id: pending.request_id, path: req.path,
      });
      const data = await pending.promise;
      return create(FilesMkdirResponseSchema, { resolvedPath: data.resolved_path ?? req.path });
    },

    // att1-stream — one bounded chunk of a chunked upload (no size ceiling).
    // Stateless across calls: resolves the session's worker per chunk and
    // relays a DAttachmentChunk (raw bytes). The worker assembles by upload_id
    // into a temp file. On `last`, coord registers a pending under upload_id
    // (so the worker's rpc-ok resolves it), sends the final chunk, and returns
    // abs_path; non-last chunks return immediately with an empty path. Memory
    // here is O(chunk), not O(file).
    async attachFileChunk(req, ctx) {
      requireAuth(ctx.values);  // authz gate (throws if unauthed)
      if (!req.uploadId) throw new ConnectError("upload_id required", Code.InvalidArgument);
      if (!req.sessionId) throw new ConnectError("session_id required", Code.InvalidArgument);
      const { row } = await requireSessionWorkerSocket(deps.db, req.sessionId);
      const workerFp = row.worker_fp;

      // Register the pending BEFORE sending the final chunk so the worker's
      // rpc-ok can't race ahead of the pending entry. 5 min deadline covers a
      // multi-GB tailnet upload. Non-last chunks fire-and-forget (empty reply).
      const pending = req.last
        ? createPendingRpcWithId<{ abs_path: string }>(req.uploadId, 300_000, workerFp)
        : null;
      if (!sendAttachmentChunk(workerFp, {
        requestId: req.uploadId, sessionId: req.sessionId, filename: req.filename,
        shortPath: req.shortPath, data: req.data, last: req.last, seq: req.seq,
      })) throw new ConnectError("worker disconnected mid-upload", Code.Unavailable);
      if (!pending) return create(AttachFileChunkResponseSchema, { absPath: "" });
      const res = await pending.promise;
      return create(AttachFileChunkResponseSchema, { absPath: res.abs_path });
    },

    // att3 — content-dedup probe. Resolve the session's worker and relay the
    // hash; a hit returns the existing path so the SPA skips the byte upload.
    async attachmentProbe(req, ctx) {
      const caller = requireAuth(ctx.values);
      if (!req.sessionId) throw new ConnectError("session_id required", Code.InvalidArgument);
      const { row, sock } = await requireSessionWorkerSocket(deps.db, req.sessionId);
      const pending = createPendingRpc<{ hit: boolean; abs_path: string }>(10_000, row.worker_fp);
      sendBrowserCmd(sock, caller, pending.request_id, {
        kind: "attachment-probe" as const, request_id: pending.request_id, session_id: asSessionId(req.sessionId), sha256: req.sha256, short_path: req.shortPath,
      });
      const data = await pending.promise;
      return create(AttachmentProbeResponseSchema, { hit: data.hit, absPath: data.abs_path });
    },

    async listAttachments(req, ctx) {
      const caller = requireAuth(ctx.values);
      const { row, sock } = await requireSessionWorkerSocket(deps.db, req.sessionId);
      const pending = createPendingRpc<{ entries: Array<{ filename: string; size_bytes: number; mtime_ms: number; abs_path: string }> }>(10_000, row.worker_fp);
      sendBrowserCmd(sock, caller, pending.request_id, {
        kind: "list-attachments" as const,
        request_id: pending.request_id,
        session_id: asSessionId(req.sessionId),
      });
      const data = await pending.promise;
      return create(ListAttachmentsResponseSchema, {
        entries: data.entries.map(e => create(AttachmentEntrySchema, {
          filename: e.filename,
          sizeBytes: BigInt(e.size_bytes),
          mtimeMs: BigInt(Math.trunc(e.mtime_ms)),
          absPath: e.abs_path,
        })),
      });
    },

    async deleteAttachment(req, ctx) {
      const caller = requireAuth(ctx.values);
      const { row, sock } = await requireSessionWorkerSocket(deps.db, req.sessionId);
      const pending = createPendingRpc<{ ok: boolean }>(10_000, row.worker_fp);
      sendBrowserCmd(sock, caller, pending.request_id, {
        kind: "delete-attachment" as const,
        request_id: pending.request_id,
        session_id: asSessionId(req.sessionId),
        filename: req.filename,
      });
      const data = await pending.promise;
      return create(DeleteAttachmentResponseSchema, { ok: data.ok });
    },
  };
}
