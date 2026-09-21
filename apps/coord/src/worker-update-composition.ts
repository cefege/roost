// Process composition for the durable POSIX worker-update owner. Database rows
// supply restart-safe baselines and confirmation evidence; operation changes
// publish one full registered Worker row through the existing presence domain.

import { KeeperRuntimeObservationV1Schema } from "@roost/shared/keeper-update";
import { safeJsonParse } from "@roost/shared/json";
import { workerRowToWirePresence } from "@roost/shared/wire/row-proto";
import type { Worker as WireWorker } from "@roost/shared/wire";
import type { CoordConfig } from "@roost/shared/config";
import { resolveCoordinatorDialUrl } from "@roost/shared/coordinator-dial-url";
import { presenceBus } from "./buses.ts";
import type { KyselyDB } from "./db/connection.ts";
import { connectWorkers, listRoutableFps } from "./connect/worker-registry.ts";
import { WorkerUpdateOwner } from "./worker-update-owner.ts";
import {
  loadDeployJobRecord,
  type WorkerUpdateBaseline,
} from "./deploy-job-record.ts";
import type { startDeployJobRuntime } from "./deploy-job-runtime.ts";

export interface WorkerUpdateComposition {
  owner: WorkerUpdateOwner;
  sourceRoot: string;
}

export function createWorkerUpdateComposition(
  db: KyselyDB,
  cfg: CoordConfig,
  options: { startRuntime?: typeof startDeployJobRuntime } = {},
): WorkerUpdateComposition {
  const sourceRoot = process.env.ROOST_REPO_ROOT?.trim() || process.cwd();
  const coordinatorDialUrl = resolveCoordinatorDialUrl(process.env)
    ?? cfg.publicUrl
    ?? "http://127.0.0.1:4103";
  const coordinatorOrigin = sanitizedCoordinatorOrigin(coordinatorDialUrl);
  let owner: WorkerUpdateOwner;
  owner = new WorkerUpdateOwner({
    coordinatorOrigin,
    coordinatorDialUrl,
    readBaseline: workerFp => readWorkerUpdateBaseline(db, workerFp),
    readVerification: async (workerFp) => {
      const operation = owner.readSummary(workerFp);
      if (!operation) return null;
      const loaded = await loadDeployJobRecord(workerFp, operation.jobId);
      if (loaded.kind !== "record") return null;
      const worker = await db.selectFrom("workers")
        .select(["git_sha", "last_seen_ms", "keeper_runtime_json"])
        .where("fp", "=", workerFp)
        .where("deleted_at_ms", "is", null)
        .executeTakeFirst();
      if (!worker) return null;
      const currentKeeper = parseKeeperRuntime(worker.keeper_runtime_json);
      const sessions = await db.selectFrom("sessions")
        .select(["id", "status", "closed_at"])
        .where("worker_fp", "=", workerFp)
        .where("id", "in", [...loaded.record.baseline.sessionIds])
        .execute();
      const sessionsPreserved = loaded.record.baseline.sessionIds.every((sessionId) => {
        const row = sessions.find(candidate => candidate.id === sessionId);
        return row?.status === "open"
          || (row?.status === "closed"
            && row.closed_at !== null
            && row.closed_at >= loaded.record.baseline.heartbeatAtMs);
      });
      const keeperPreserved = loaded.record.baseline.keeperPid === null
        || (currentKeeper !== null
          && currentKeeper.keeper_pid === loaded.record.baseline.keeperPid
          && currentKeeper.keeper_epoch === loaded.record.baseline.keeperEpoch
          && currentKeeper.binding_digest === loaded.record.baseline.bindingDigest);
      return {
        routable: listRoutableFps().includes(workerFp),
        heartbeatAtMs: worker.last_seen_ms,
        gitSha: worker.git_sha,
        journalSettled: loaded.record.report.events.some((event) =>
          event.phase === "settled" && event.message === "Host journal settled"),
        keeperConverged: keeperPreserved && sessionsPreserved,
      };
    },
    publishOperation: async (workerFp, operation) => {
      const row = await db.selectFrom("workers")
        .selectAll()
        .where("fp", "=", workerFp)
        .where("deleted_at_ms", "is", null)
        .executeTakeFirst();
      if (!row) return;
      presenceBus.publish({
        kind: "registered",
        worker: workerRowToWirePresence(row, operation) as unknown as WireWorker,
      });
    },
    workerExists: async (workerFp) => Boolean(await db.selectFrom("workers")
      .select("fp")
      .where("fp", "=", workerFp)
      .where("deleted_at_ms", "is", null)
      .executeTakeFirst()),
    workerRoutable: workerFp => listRoutableFps().includes(workerFp),
    startRuntime: options.startRuntime,
  });
  return { owner, sourceRoot };
}

async function readWorkerUpdateBaseline(
  db: KyselyDB,
  workerFp: string,
): Promise<WorkerUpdateBaseline> {
  const worker = await db.selectFrom("workers")
    .select(["git_sha", "last_seen_ms", "keeper_runtime_json"])
    .where("fp", "=", workerFp)
    .where("deleted_at_ms", "is", null)
    .executeTakeFirstOrThrow();
  const sessions = await db.selectFrom("sessions")
    .select("id")
    .where("worker_fp", "=", workerFp)
    .where("status", "=", "open")
    .orderBy("id", "asc")
    .execute();
  const keeper = parseKeeperRuntime(worker.keeper_runtime_json);
  return {
    heartbeatAtMs: worker.last_seen_ms,
    processEpoch: connectWorkers.get(workerFp)?.processEpoch ?? null,
    gitSha: worker.git_sha,
    keeperPid: keeper?.keeper_pid ?? null,
    keeperEpoch: keeper?.keeper_epoch ?? null,
    bindingDigest: keeper?.binding_digest ?? null,
    sessionIds: sessions.map(session => session.id),
  };
}

function parseKeeperRuntime(serialized: string | null): ReturnType<
  typeof KeeperRuntimeObservationV1Schema.parse
> | null {
  if (!serialized) return null;
  const parsed = KeeperRuntimeObservationV1Schema.safeParse(
    safeJsonParse(serialized, null, "keeper_runtime_json"),
  );
  return parsed.success ? parsed.data : null;
}

export function sanitizedCoordinatorOrigin(value: string): string {
  const url = new URL(value);
  if (url.username || url.password || url.search || url.hash
    || (url.pathname !== "" && url.pathname !== "/")) {
    throw new Error("coordinator report origin must not contain credentials or URL state");
  }
  return url.origin;
}
