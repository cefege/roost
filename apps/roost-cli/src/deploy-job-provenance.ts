// Validates the coordinator-owned private job record before internal pinned
// source deployment bypasses ordinary upstream publication checks. Every host,
// worker, source, target and job identity must match before target probing.

import { lstatSync, readFileSync, realpathSync } from "node:fs";
import { join, resolve } from "node:path";
import { coordDataDir } from "@roost/shared/paths";
import {
  WorkerUpdateOperationSchema,
  type WorkerUpdateOperation,
} from "@roost/shared/worker-update-operation";
import { DeployFailure } from "./deploy-exec.ts";

interface JobRecordProvenance {
  schemaVersion: 1;
  operation: WorkerUpdateOperation;
  sourceRoot: string;
  sourceMode: "coordinator-pinned";
}
const JOB_ID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const WORKER_FP_RE = /^[0-9a-f]{64}$/;

export function validatePinnedDeployJobOrDie(input: {
  jobId: string;
  workerFp: string | undefined;
  host: string;
  sourceRoot: string;
  targetSha: string;
}): void {
  if (!JOB_ID_RE.test(input.jobId) || !input.workerFp
    || !WORKER_FP_RE.test(input.workerFp)) {
    throw provenanceFailure("pinned deploy job identity is invalid");
  }
  const path = join(
    coordDataDir(),
    "deploy-jobs",
    input.workerFp,
    `${input.jobId}.json`,
  );
  let serialized: string;
  try {
    const metadata = lstatSync(path);
    if (!metadata.isFile() || metadata.isSymbolicLink() || (metadata.mode & 0o077) !== 0) {
      throw new Error("record is not a private regular file");
    }
    serialized = readFileSync(path, "utf8");
  } catch (error) {
    throw provenanceFailure(`pinned deploy record is unavailable: ${String(error)}`);
  }
  if (Buffer.byteLength(serialized) > 8 * 1024 * 1024) {
    throw provenanceFailure("pinned deploy record exceeds the size limit");
  }
  let record: JobRecordProvenance;
  try {
    const value: unknown = JSON.parse(serialized);
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      throw new Error("record root must be an object");
    }
    const fields = value as Record<string, unknown>;
    if (fields.schemaVersion !== 1 || fields.sourceMode !== "coordinator-pinned"
      || typeof fields.sourceRoot !== "string" || fields.sourceRoot.length > 4_096) {
      throw new Error("record provenance fields are invalid");
    }
    record = {
      schemaVersion: 1,
      operation: WorkerUpdateOperationSchema.parse(fields.operation),
      sourceRoot: fields.sourceRoot,
      sourceMode: "coordinator-pinned",
    };
  } catch (error) {
    throw provenanceFailure(`pinned deploy record is invalid: ${String(error)}`);
  }
  const canonicalSourceRoot = canonicalRealPath(input.sourceRoot);
  if (record.operation.jobId !== input.jobId
    || record.operation.workerFp !== input.workerFp
    || record.operation.host.toLowerCase() !== input.host.toLowerCase()
    || record.operation.targetGitSha !== input.targetSha.toLowerCase()
    || record.sourceRoot !== canonicalSourceRoot
    || record.sourceMode !== "coordinator-pinned") {
    throw provenanceFailure("pinned deploy request does not match its durable coordinator record");
  }
}

function canonicalRealPath(path: string): string {
  const canonical = resolve(path);
  try {
    if (canonical !== path || realpathSync(path) !== canonical) {
      throw new Error("source root is not canonical");
    }
  } catch (error) {
    throw provenanceFailure(`pinned source root is unavailable: ${String(error)}`);
  }
  return canonical;
}

function provenanceFailure(message: string): DeployFailure {
  return new DeployFailure(7, message, {
    code: "source_unavailable",
    phase: "preflight",
    message,
    journal: null,
    expectedKeeper: null,
    observedKeeper: null,
    targetContract: null,
  });
}
