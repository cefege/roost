// Durable POSIX worker-update job records. The coordinator rebuilds the latest
// per-worker operation by monotonic revision and retains unresolved work across
// restarts; platform journals remain the mutation/recovery authority.

import { lstat, readdir, readFile, realpath } from "node:fs/promises";
import { isAbsolute, join, normalize, resolve } from "node:path";
import { z } from "zod";
import { durableWriteFile } from "@roost/shared/durability";
import { coordDataDir } from "@roost/shared/paths";
import {
  WorkerUpdateOperationSchema,
  WorkerUpdateReportSchema,
  WORKER_UPDATE_GIT_SHA_RE,
  type WorkerUpdateOperation,
  type WorkerUpdateReport,
} from "@roost/shared/worker-update-operation";

export const DEPLOY_JOB_MAX_LINES = 2_048;
export const DEPLOY_JOB_MAX_LINE_LENGTH = 2_048;
export const DEPLOY_JOB_MAX_RECORD_BYTES = 8 * 1024 * 1024;
const JOB_ID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const WORKER_FP_RE = /^[0-9a-f]{64}$/;
const SINGLE_LINE_RE = /^[^\r\n]*$/;
const SafeTimestamp = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);

const WorkerUpdateBaselineSchema = z.object({
  heartbeatAtMs: SafeTimestamp,
  processEpoch: z.string().min(1).max(256).regex(SINGLE_LINE_RE).nullable(),
  gitSha: z.string().regex(WORKER_UPDATE_GIT_SHA_RE).nullable(),
  keeperPid: z.number().int().positive().max(Number.MAX_SAFE_INTEGER).nullable(),
  keeperEpoch: z.string().uuid().nullable(),
  bindingDigest: z.string().regex(/^[0-9a-f]{64}$/).nullable(),
  sessionIds: z.array(z.string().uuid()).max(65_535).readonly(),
}).strict().readonly();
export type WorkerUpdateBaseline = z.infer<typeof WorkerUpdateBaselineSchema>;

export const PersistedDeployJobSchema = z.object({
  schemaVersion: z.literal(1),
  operation: WorkerUpdateOperationSchema,
  report: WorkerUpdateReportSchema,
  sourceRoot: z.string().min(1).max(4_096).regex(SINGLE_LINE_RE),
  sourceMode: z.literal("coordinator-pinned"),
  baseline: WorkerUpdateBaselineSchema,
  lines: z.array(z.string().min(1).max(DEPLOY_JOB_MAX_LINE_LENGTH))
    .max(DEPLOY_JOB_MAX_LINES),
}).strict().readonly().superRefine((record, context) => {
  if (record.report.operation.jobId !== record.operation.jobId
    || record.report.operation.revision !== record.operation.revision) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      path: ["report", "operation"],
      message: "report operation does not match record operation",
    });
  }
  if (!isCanonicalAbsolutePath(record.sourceRoot)) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      path: ["sourceRoot"],
      message: "sourceRoot must be a canonical absolute path",
    });
  }
});
export type PersistedDeployJob = z.infer<typeof PersistedDeployJobSchema>;

export type DeployJobRecordLoad =
  | { kind: "record"; record: PersistedDeployJob }
  | { kind: "missing" | "invalid"; error?: string };

export function isDeployJobId(value: string): boolean {
  return JOB_ID_RE.test(value);
}

export function isWorkerFingerprint(value: string): boolean {
  return WORKER_FP_RE.test(value);
}

export function deployJobDirectory(workerFp: string): string {
  if (!isWorkerFingerprint(workerFp)) throw new Error("invalid deploy worker fingerprint");
  return join(coordDataDir(), "deploy-jobs", workerFp);
}

export function deployJobRecordPath(workerFp: string, jobId: string): string {
  if (!isDeployJobId(jobId)) throw new Error("invalid deploy job ID");
  return join(deployJobDirectory(workerFp), `${jobId}.json`);
}

export function normalizeDeployOutputLine(text: string): string | null {
  const line = text.replace(/[\r\n]+/g, " ").trim().slice(0, DEPLOY_JOB_MAX_LINE_LENGTH);
  return line || null;
}

export function appendDeployOutputLine(lines: readonly string[], line: string): string[] {
  const start = Math.max(0, lines.length - DEPLOY_JOB_MAX_LINES + 1);
  return [...lines.slice(start), line];
}

export async function persistDeployJobRecord(record: PersistedDeployJob): Promise<void> {
  const checked = PersistedDeployJobSchema.parse(record);
  const serialized = `${JSON.stringify(checked)}\n`;
  if (Buffer.byteLength(serialized) > DEPLOY_JOB_MAX_RECORD_BYTES) {
    throw new Error("deploy job record exceeds the maximum size");
  }
  await durableWriteFile(
    deployJobRecordPath(checked.operation.workerFp, checked.operation.jobId),
    serialized,
    { mode: 0o600, privateDacl: true },
  );
}

export async function loadDeployJobRecord(
  workerFp: string,
  jobId: string,
): Promise<DeployJobRecordLoad> {
  let raw: string;
  try {
    raw = await readFile(deployJobRecordPath(workerFp, jobId), "utf8");
  } catch (error) {
    const code = error instanceof Error && "code" in error
      ? (error as NodeJS.ErrnoException).code
      : undefined;
    return { kind: code === "ENOENT" ? "missing" : "invalid", error: String(error) };
  }
  if (Buffer.byteLength(raw) > DEPLOY_JOB_MAX_RECORD_BYTES) {
    return { kind: "invalid", error: "record exceeds the maximum size" };
  }
  try {
    const parsed = PersistedDeployJobSchema.parse(JSON.parse(raw));
    if (parsed.operation.workerFp !== workerFp || parsed.operation.jobId !== jobId) {
      return { kind: "invalid", error: "record identity does not match its path" };
    }
    return { kind: "record", record: parsed };
  } catch (error) {
    return { kind: "invalid", error: String(error) };
  }
}

export interface LoadedDeployJobRecords {
  records: PersistedDeployJob[];
  corruptWorkerFingerprints: Map<string, string[]>;
}

export async function loadAllDeployJobRecords(): Promise<LoadedDeployJobRecords> {
  const root = join(coordDataDir(), "deploy-jobs");
  const records: PersistedDeployJob[] = [];
  const corruptWorkerFingerprints = new Map<string, string[]>();
  let workers: string[];
  try {
    workers = await readdir(root);
  } catch (error) {
    const code = error instanceof Error && "code" in error
      ? (error as NodeJS.ErrnoException).code
      : undefined;
    if (code === "ENOENT") return { records, corruptWorkerFingerprints };
    throw error;
  }
  for (const workerFp of workers) {
    if (!isWorkerFingerprint(workerFp)) continue;
    const directory = join(root, workerFp);
    try {
      const metadata = await lstat(directory);
      if (!metadata.isDirectory() || metadata.isSymbolicLink()
        || await realpath(directory) !== resolve(directory)) {
        corruptWorkerFingerprints.set(workerFp, [directory]);
        continue;
      }
      const names = await readdir(directory);
      for (const name of names) {
        const jobId = name.endsWith(".json") ? name.slice(0, -5) : "";
        if (!isDeployJobId(jobId)) continue;
        const loaded = await loadDeployJobRecord(workerFp, jobId);
        if (loaded.kind === "record") records.push(loaded.record);
        else if (loaded.kind === "invalid") {
          const corrupt = corruptWorkerFingerprints.get(workerFp) ?? [];
          corrupt.push(join(directory, name));
          corruptWorkerFingerprints.set(workerFp, corrupt);
        }
      }
    } catch (error) {
      corruptWorkerFingerprints.set(workerFp, [String(error)]);
    }
  }
  return { records, corruptWorkerFingerprints };
}

export function latestDeployOperations(
  records: readonly PersistedDeployJob[],
): Map<string, WorkerUpdateOperation> {
  const latest = new Map<string, WorkerUpdateOperation>();
  for (const record of records) {
    const current = latest.get(record.operation.workerFp);
    if (!current || record.operation.revision > current.revision) {
      latest.set(record.operation.workerFp, record.operation);
    }
  }
  return latest;
}

function isCanonicalAbsolutePath(value: string): boolean {
  return isAbsolute(value) && normalize(value) === value && !value.includes("\0");
}
