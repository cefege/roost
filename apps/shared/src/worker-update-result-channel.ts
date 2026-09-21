// Private local result channel from the coordinator-owned deploy child to its
// durable owner. Remote stdout/stderr never enters structured report metadata;
// exact worker/job/attempt paths bind each strict terminal failure.

import { lstat, readFile } from "node:fs/promises";
import { join } from "node:path";
import { z } from "zod";
import { coordDataDir } from "./paths.ts";
import { durableRemove, durableWriteFile } from "./durability.ts";
import { WorkerUpdateFailureSchema, type WorkerUpdateFailure } from "./worker-update-operation.ts";

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const WORKER_FP_RE = /^[0-9a-f]{64}$/;
const WorkerUpdateChildResultSchema = z.object({
  schemaVersion: z.literal(1),
  jobId: z.string().regex(UUID_RE),
  resultId: z.string().regex(UUID_RE),
  workerFp: z.string().regex(WORKER_FP_RE),
  failure: WorkerUpdateFailureSchema,
}).strict();

export function workerUpdateChildResultPath(
  workerFp: string,
  jobId: string,
  resultId: string,
): string {
  if (!WORKER_FP_RE.test(workerFp) || !UUID_RE.test(jobId) || !UUID_RE.test(resultId)) {
    throw new Error("invalid worker update child result identity");
  }
  return join(coordDataDir(), "deploy-results", workerFp, `${jobId}-${resultId}.json`);
}

export async function persistWorkerUpdateChildFailure(input: {
  workerFp: string;
  jobId: string;
  resultId: string;
  failure: WorkerUpdateFailure;
}): Promise<void> {
  const result = WorkerUpdateChildResultSchema.parse({ schemaVersion: 1, ...input });
  await durableWriteFile(
    workerUpdateChildResultPath(result.workerFp, result.jobId, result.resultId),
    `${JSON.stringify(result)}\n`,
    { mode: 0o600, privateDacl: true },
  );
}

export async function consumeWorkerUpdateChildFailure(
  workerFp: string,
  jobId: string,
  resultId: string,
): Promise<WorkerUpdateFailure | null> {
  const path = workerUpdateChildResultPath(workerFp, jobId, resultId);
  try {
    const metadata = await lstat(path);
    if (!metadata.isFile() || metadata.isSymbolicLink() || (metadata.mode & 0o077) !== 0
      || metadata.size > 256 * 1024) return null;
    const parsed = WorkerUpdateChildResultSchema.parse(JSON.parse(await readFile(path, "utf8")));
    if (parsed.workerFp !== workerFp || parsed.jobId !== jobId
      || parsed.resultId !== resultId) return null;
    return parsed.failure;
  } catch {
    return null;
  } finally {
    await durableRemove(path, { mode: 0o600, privateDacl: true }).catch(() => undefined);
  }
}
