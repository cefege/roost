// Durable per-upload state for direct ACK recovery and terminal coordinator outcomes.
// Records live beneath the owning session's attachment directory and are synced
// before any direct receipt is sent, so a worker restart retains one outcome.

import fs from "node:fs";
import path from "node:path";
import { z } from "zod";
import {
  ATTACHMENT_OPERATION_DIR_NAME,
  resolveSessionDirWithinBase,
} from "./attachment-reaper.ts";
import {
  syncAttachmentDirectory,
  syncAttachmentDirectoryAsync,
  syncAttachmentFileAsync,
} from "./attachment-file-store.ts";

export type AttachmentOperationCarrier = "coordinator" | "direct";

export interface AttachmentOperationDescriptor {
  readonly requestId: string;
  readonly sessionId: string;
  readonly filename: string;
  readonly shortPath: boolean;
  readonly totalBytes: number | undefined;
}

export interface AttachmentOperationJournal extends AttachmentOperationDescriptor {
  readonly version: 1;
  readonly carrier: AttachmentOperationCarrier;
  readonly carrierId: string;
  nextSeq: number;
  bytesWritten: number;
  lastChunkFinal: boolean;
  lastChunkSha256: string;
  finalName: string;
  contentSha256: string;
  committed: boolean;
  absPath: string;
  error: string;
}

export interface AttachmentOperationPaths {
  readonly sessionDir: string;
  readonly operationDir: string;
  readonly journalPath: string;
  readonly tempPath: string;
}

export type AttachmentOperationLoad =
  | { readonly kind: "missing" }
  | { readonly kind: "invalid" }
  | { readonly kind: "loaded"; readonly paths: AttachmentOperationPaths; readonly journal: AttachmentOperationJournal };

const SerializedOperationJournalSchema = z.object({
  version: z.literal(1),
  requestId: z.string(),
  sessionId: z.string(),
  filename: z.string(),
  shortPath: z.boolean(),
  totalBytes: z.number().nullable(),
  lastChunkFinal: z.boolean(),
  carrier: z.enum(["coordinator", "direct"]),
  carrierId: z.string(),
  nextSeq: z.number(),
  bytesWritten: z.number(),
  lastChunkSha256: z.string(),
  finalName: z.string(),
  contentSha256: z.string(),
  committed: z.boolean(),
  absPath: z.string(),
  error: z.string(),
});

export function createAttachmentOperationPaths(sessionId: string, requestId: string): AttachmentOperationPaths | null {
  if (!validOperationId(requestId)) return null;
  const sessionDir = resolveSessionDirWithinBase(sessionId);
  if (!sessionDir) return null;
  const operationDir = path.join(sessionDir, ATTACHMENT_OPERATION_DIR_NAME);
  return {
    sessionDir,
    operationDir,
    journalPath: path.join(operationDir, `${requestId}.json`),
    tempPath: path.join(operationDir, `${requestId}.part`),
  };
}

export function loadAttachmentOperation(sessionId: string, requestId: string): AttachmentOperationLoad {
  const paths = createAttachmentOperationPaths(sessionId, requestId);
  if (!paths || !fs.existsSync(paths.journalPath)) return { kind: "missing" };
  try {
    const journal = parseJournal(fs.readFileSync(paths.journalPath, "utf8"));
    if (
      !journal
      || journal.sessionId !== sessionId
      || journal.requestId !== requestId
      || !validReceiptPath(paths.sessionDir, journal.absPath)
    ) return { kind: "invalid" };
    return { kind: "loaded", paths, journal };
  } catch {
    return { kind: "invalid" };
  }
}

export function createAttachmentOperation(
  descriptor: AttachmentOperationDescriptor,
  carrier: AttachmentOperationCarrier,
  carrierId: string,
): { paths: AttachmentOperationPaths; journal: AttachmentOperationJournal } | null {
  const paths = createAttachmentOperationPaths(descriptor.sessionId, descriptor.requestId);
  if (!paths) return null;
  fs.mkdirSync(paths.operationDir, { recursive: true, mode: 0o700 });
  // Explicit fields: callers pass whole chunks, and spreading one would serialize its bytes.
  const journal: AttachmentOperationJournal = {
    version: 1,
    requestId: descriptor.requestId,
    sessionId: descriptor.sessionId,
    filename: descriptor.filename,
    shortPath: descriptor.shortPath,
    totalBytes: descriptor.totalBytes,
    lastChunkFinal: false,
    carrier,
    carrierId,
    nextSeq: 0,
    bytesWritten: 0,
    lastChunkSha256: "",
    finalName: "",
    contentSha256: "",
    committed: false,
    absPath: "",
    error: "",
  };
  // Unsynced: relay progress is never durable, and a direct ACK flushes this journal first.
  persistAttachmentOperation(paths, journal, false);
  return { paths, journal };
}

export function persistAttachmentOperation(
  paths: AttachmentOperationPaths,
  journal: AttachmentOperationJournal,
  sync = true,
): void {
  fs.mkdirSync(paths.operationDir, { recursive: true, mode: 0o700 });
  const pendingPath = `${paths.journalPath}.next`;
  fs.writeFileSync(pendingPath, JSON.stringify({ ...journal, totalBytes: journal.totalBytes ?? null }), { mode: 0o600 });
  if (sync) syncFile(pendingPath);
  fs.renameSync(pendingPath, paths.journalPath);
  if (sync) syncAttachmentDirectory(paths.operationDir);
}

/** Flushes one accepted direct chunk without blocking the worker event loop. */
export async function syncAttachmentOperationProgress(
  sessionId: string,
  requestId: string,
): Promise<void> {
  const paths = createAttachmentOperationPaths(sessionId, requestId);
  if (!paths) throw new Error("attachment operation is unavailable");
  // Every flush must land before the ACK, but none orders another, so they run together.
  await Promise.all([
    syncAttachmentFileAsync(paths.tempPath),
    syncAttachmentFileAsync(paths.journalPath),
    syncAttachmentDirectoryAsync(paths.operationDir),
    syncAttachmentDirectoryAsync(paths.sessionDir),
  ]);
}

export function removeAttachmentTemp(paths: AttachmentOperationPaths): void {
  try { fs.unlinkSync(paths.tempPath); } catch { /* absent temp is already terminal */ }
}

function parseJournal(value: string): AttachmentOperationJournal | null {
  const parsed = SerializedOperationJournalSchema.safeParse(JSON.parse(value));
  if (!parsed.success) return null;
  const journal = parsed.data;
  if (
    !validOperationId(journal.requestId)
    || !validText(journal.sessionId)
    || !validText(journal.filename)
    || !validOptionalTotal(journal.totalBytes)
    || !validCounter(journal.nextSeq)
    || !validCounter(journal.bytesWritten)
    || !validCarrierId(journal.carrierId)
    || !validDigest(journal.lastChunkSha256)
    || !validFinalName(journal.finalName)
    || !validDigest(journal.contentSha256)
  ) return null;
  return {
    version: 1,
    requestId: journal.requestId,
    sessionId: journal.sessionId,
    filename: journal.filename,
    shortPath: journal.shortPath,
    totalBytes: journal.totalBytes === null ? undefined : journal.totalBytes,
    carrier: journal.carrier,
    lastChunkFinal: journal.lastChunkFinal,
    carrierId: journal.carrierId,
    nextSeq: journal.nextSeq,
    bytesWritten: journal.bytesWritten,
    lastChunkSha256: journal.lastChunkSha256,
    finalName: journal.finalName,
    contentSha256: journal.contentSha256,
    committed: journal.committed,
    absPath: journal.absPath,
    error: journal.error,
  };
}

function syncFile(filePath: string): void {
  const fd = fs.openSync(filePath, "r");
  try { fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
}

function validOperationId(value: string): boolean {
  const bytes = Buffer.byteLength(value, "utf8");
  return bytes > 0 && bytes <= 128 && !value.includes("/") && !value.includes("\\") && !/[\x00-\x1f\x7f]/.test(value);
}

function validText(value: string): boolean {
  return Buffer.byteLength(value, "utf8") <= 1_024;
}

function validOptionalTotal(value: number | null): boolean {
  return value === null || Number.isSafeInteger(value) && value >= 0;
}

function validCounter(value: number): boolean {
  return Number.isSafeInteger(value) && value >= 0;
}

function validCarrierId(value: string): boolean {
  return Buffer.byteLength(value, "utf8") <= 128 && !/[\x00-\x1f\x7f]/.test(value);
}

function validDigest(value: string): boolean {
  return value === "" || /^[0-9a-f]{64}$/.test(value);
}

function validFinalName(value: string): boolean {
  return value === "" || (
    Buffer.byteLength(value, "utf8") <= 255
    && value !== "."
    && value !== ".."
    && !value.includes("/")
    && !value.includes("\\")
    && !/[\x00-\x1f\x7f]/.test(value)
  );
}

function validReceiptPath(sessionDir: string, value: string): boolean {
  if (value === "") return true;
  const resolvedSessionDir = path.resolve(sessionDir);
  const resolvedPath = path.resolve(value);
  return resolvedPath === resolvedSessionDir || resolvedPath.startsWith(`${resolvedSessionDir}${path.sep}`);
}
