// Durable record layer for signed Windows-update deploy jobs: the on-disk
// schema, its validation, and the read/write helpers. Split out of
// deploy-jobs.ts (400-line cap), which keeps the generic DeployJob registry
// and the POSIX deploy; windows-update-deploy-jobs.ts owns the job
// bookkeeping and windows-update-deploy-runtime.ts the live job runtime.

import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { z } from "zod";

import { coordDataDir } from "@roost/shared/paths";
import { durableRemove, durableWriteFile } from "@roost/shared/durability";
import { DEPLOY_JOB_TTL_MS, type DeployJob } from "./deploy-jobs.ts";

export const WINDOWS_UPDATE_TIMEOUT_MS = 15 * 60 * 1000;
export const WINDOWS_UPDATE_POLL_MS = 2_000;
const MAX_PERSISTED_UPDATE_LINES = 2_048;
const MAX_PERSISTED_UPDATE_LINE_LENGTH = 2_048;
const MAX_PERSISTED_UPDATE_RECORD_BYTES = 8 * 1024 * 1024;
const MAX_PERSISTED_TIMESTAMP_SKEW_MS = 5 * 60 * 1000;
// A deploy jobId is a crypto.randomUUID(). Defined here, not imported from
// deploy-jobs.ts: the zod schema below reads it at module-evaluation time and
// deploy-jobs.ts imports back into this module, so a cross-module const would
// hit the temporal dead zone whenever deploy-jobs.ts is the cycle entry point.
const DEPLOY_JOB_ID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
export const WINDOWS_WORKER_FP_RE = /^[0-9a-f]{64}$/;
const SHA256_RE = /^[0-9a-f]{64}$/;
export const HOST_RE = /^[A-Za-z0-9.-]+$/;

export const httpsUrlSchema = z.string().min(1).max(2_048).url().refine((value) => {
  try {
    const url = new URL(value);
    return url.protocol === "https:" && !url.username && !url.password;
  } catch {
    return false;
  }
}, "URL must use HTTPS without credentials");

const persistedWindowsUpdateCommon = {
  schemaVersion: z.literal(1),
  jobId: z.string().regex(DEPLOY_JOB_ID_RE),
  host: z.string().min(1).max(253).regex(HOST_RE),
  workerFp: z.string().regex(WINDOWS_WORKER_FP_RE),
  manifestUrl: httpsUrlSchema,
  signatureUrl: httpsUrlSchema,
  manifestSha256: z.string().regex(SHA256_RE),
  publisherSha256: z.union([z.literal(""), z.string().regex(SHA256_RE)]),
  startedAt: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER),
  updatedAt: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER),
  startAccepted: z.boolean(),
  lastSequence: z.number().int().min(-1).max(Number.MAX_SAFE_INTEGER),
  lines: z.array(z.string().min(1).max(MAX_PERSISTED_UPDATE_LINE_LENGTH))
    .max(MAX_PERSISTED_UPDATE_LINES),
};

const persistedWindowsUpdateRunningSchema = z.object({
  ...persistedWindowsUpdateCommon,
  status: z.literal("running"),
  completedAt: z.null(),
  exitCode: z.null(),
  error: z.null(),
}).strict();

const persistedWindowsUpdateDoneSchema = z.object({
  ...persistedWindowsUpdateCommon,
  status: z.literal("done"),
  completedAt: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER),
  exitCode: z.union([z.literal(0), z.literal(1)]),
  error: z.string().min(1).max(MAX_PERSISTED_UPDATE_LINE_LENGTH).nullable(),
}).strict();

const persistedWindowsUpdateSchema = z.discriminatedUnion("status", [
  persistedWindowsUpdateRunningSchema,
  persistedWindowsUpdateDoneSchema,
]).superRefine((record, context) => {
  if (record.updatedAt < record.startedAt) {
    context.addIssue({ code: z.ZodIssueCode.custom, message: "updatedAt precedes startedAt" });
  }
  if (record.manifestUrl === record.signatureUrl) {
    context.addIssue({ code: z.ZodIssueCode.custom, message: "manifest URLs must be distinct" });
  }
  if (record.lastSequence >= 0 && !record.startAccepted) {
    context.addIssue({ code: z.ZodIssueCode.custom, message: "progress requires accepted START" });
  }
  if (record.status === "done"
    && (record.completedAt < record.startedAt || record.updatedAt < record.completedAt)) {
    context.addIssue({ code: z.ZodIssueCode.custom, message: "invalid terminal timestamps" });
  }
  if (record.status === "done"
    && ((record.exitCode === 0 && record.error !== null)
      || (record.exitCode === 1 && record.error === null))) {
    context.addIssue({ code: z.ZodIssueCode.custom, message: "terminal result is inconsistent" });
  }
});

export type PersistedWindowsUpdateJob = z.infer<typeof persistedWindowsUpdateSchema>;
export type PersistedWindowsUpdateLoad =
  | { kind: "record"; record: PersistedWindowsUpdateJob }
  | { kind: "missing" | "invalid" | "expired" };

export function isDeployJobId(jobId: string): boolean {
  return DEPLOY_JOB_ID_RE.test(jobId);
}

export function windowsUpdateDeployRecordPath(jobId: string): string {
  if (!isDeployJobId(jobId)) throw new Error("invalid Windows update jobId");
  return join(coordDataDir(), "windows-update-jobs", `${jobId}.json`);
}

export function normalizeWindowsUpdateLine(text: string): string | null {
  const line = text.replace(/\r?\n/g, " ").trim().slice(0, MAX_PERSISTED_UPDATE_LINE_LENGTH);
  return line || null;
}

export function linesWithWindowsUpdateLine(lines: readonly string[], line: string): string[] {
  const start = Math.max(0, lines.length - MAX_PERSISTED_UPDATE_LINES + 1);
  return [...lines.slice(start), line];
}

export function persistedWindowsUpdateRecord(
  job: DeployJob,
  overrides: Record<string, unknown> = {},
): PersistedWindowsUpdateJob {
  const update = job.windowsUpdate;
  if (!update?.durable) throw new Error("Windows update job has no durable coordinator record");
  return persistedWindowsUpdateSchema.parse({
    schemaVersion: 1,
    jobId: job.jobId,
    host: job.host,
    workerFp: update.workerFp,
    manifestUrl: update.manifestUrl,
    signatureUrl: update.signatureUrl,
    manifestSha256: update.manifestSha256,
    publisherSha256: update.publisherSha256,
    status: job.status,
    startedAt: job.startedAt,
    updatedAt: job.updatedAt ?? job.startedAt,
    completedAt: job.completedAt ?? null,
    startAccepted: update.startAccepted,
    lastSequence: update.lastSequence,
    lines: job.lines,
    exitCode: job.exitCode ?? null,
    error: job.error ?? null,
    ...overrides,
  });
}

export async function persistWindowsUpdateRecord(record: PersistedWindowsUpdateJob): Promise<void> {
  await durableWriteFile(
    windowsUpdateDeployRecordPath(record.jobId),
    `${JSON.stringify(record)}\n`,
    { mode: 0o600, privateDacl: true },
  );
}


export async function loadPersistedWindowsUpdate(jobId: string): Promise<PersistedWindowsUpdateLoad> {
  let raw: string;
  try {
    raw = await readFile(windowsUpdateDeployRecordPath(jobId), "utf8");
  } catch (error) {
    const code = error instanceof Error && "code" in error
      ? (error as NodeJS.ErrnoException).code
      : undefined;
    return { kind: code === "ENOENT" ? "missing" : "invalid" };
  }
  if (Buffer.byteLength(raw) > MAX_PERSISTED_UPDATE_RECORD_BYTES) return { kind: "invalid" };
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return { kind: "invalid" };
  }
  const parsed = persistedWindowsUpdateSchema.safeParse(value);
  if (!parsed.success || parsed.data.jobId !== jobId) return { kind: "invalid" };
  const record = parsed.data;
  const latestTimestamp = record.status === "done"
    ? Math.max(record.startedAt, record.updatedAt, record.completedAt)
    : Math.max(record.startedAt, record.updatedAt);
  if (latestTimestamp > Date.now() + MAX_PERSISTED_TIMESTAMP_SKEW_MS) return { kind: "invalid" };
  const expiresAt = record.status === "done"
    ? record.completedAt + DEPLOY_JOB_TTL_MS
    : record.startedAt + WINDOWS_UPDATE_TIMEOUT_MS + DEPLOY_JOB_TTL_MS;
  if (expiresAt <= Date.now()) {
    await durableRemove(windowsUpdateDeployRecordPath(jobId), {
      mode: 0o600,
      privateDacl: true,
    });
    return { kind: "expired" };
  }
  return { kind: "record", record };
}
