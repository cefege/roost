// Coordinator authority for durable POSIX worker updates. It serializes one
// mutation per worker and normalized host, persists before publication/spawn,
// bounds global execution, and recovers unresolved jobs with their original ID.
import { BoundedBus } from "./buses.ts";
import { busToAsyncIterable } from "./sse.ts";
import { log } from "@roost/shared/log";
import {
  WorkerUpdateStartRequest,
  type WorkerUpdateFailure,
  type WorkerUpdateOperation,
  type WorkerUpdateProgressEvent,
  type WorkerUpdateReport,
} from "@roost/shared/worker-update-operation";
import {
  appendDeployOutputLine,
  latestDeployOperations,
  loadAllDeployJobRecords,
  loadDeployJobRecord,
  persistDeployJobRecord,
  type PersistedDeployJob,
} from "./deploy-job-record.ts";
import {
  startDeployJobRuntime,
  type DeployJobRuntimeHandle,
} from "./deploy-job-runtime.ts";
import type { DeployStreamMsg } from "./deploy-job-stream.ts";
import {
  MAX_ACTIVE_POSIX_JOBS, WORKER_UPDATE_RETRY_DELAY_MS, WORKER_UPDATE_VERIFY_POLL_MS, WORKER_UPDATE_VERIFY_TIMEOUT_MS,
  canonicalCoordinatorSourceRoot, initialDeployJobRecord, classifyWorkerUpdateFailure, finishedDeployJobRecord,
  initialWorkerUpdateOperation, interruptedDeployJobRecord, offlineDeployJobRecord, nextDeployJobRecord,
  terminalDeployFrame, validateWorkerUpdateStart, workerUpdateStatusIsTerminal,
  type DeployStartResult, type WorkerUpdateOwnerDeps,
} from "./worker-update-owner-types.ts";
export type { DeployStartResult, WorkerUpdateOwnerDeps, WorkerUpdateVerification } from "./worker-update-owner-types.ts";
interface LiveDeployJob {
  record: PersistedDeployJob;
  bus: BoundedBus<DeployStreamMsg>;
  runtime: DeployJobRuntimeHandle | null;
  mutationTail: Promise<void>;
}
export class WorkerUpdateOwner {
  readonly #deps: WorkerUpdateOwnerDeps;
  readonly #jobs = new Map<string, LiveDeployJob>();
  readonly #latestByWorker = new Map<string, WorkerUpdateOperation>();
  readonly #unresolvedJobByWorker = new Map<string, string>();
  readonly #unresolvedJobByHost = new Map<string, string>();
  readonly #queue: string[] = [];
  readonly #corruptWorkers = new Map<string, string[]>();
  readonly #executions = new Set<Promise<void>>();
  #activeCount = 0;
  #accepting = true;
  constructor(deps: WorkerUpdateOwnerDeps) {
    this.#deps = deps;
  }
  async initialize(): Promise<void> {
    const loaded = await loadAllDeployJobRecords();
    for (const [workerFp, paths] of loaded.corruptWorkerFingerprints) this.#corruptWorkers.set(workerFp, paths);
    const recoverableRecords = loaded.records.filter(record => !this.#corruptWorkers.has(record.operation.workerFp));
    for (const [workerFp, operation] of latestDeployOperations(recoverableRecords)) {
      this.#latestByWorker.set(workerFp, operation);
    }
    for (const record of recoverableRecords) {
      const job = this.#installRecord(record);
      if (!workerUpdateStatusIsTerminal(record.operation.status)) {
        this.#claimUnresolved(record.operation);
        await this.#mutate(job, record.operation.status === "waiting"
          ? record
          : nextDeployJobRecord(job.record, {
              status: "waiting",
              phase: "recovery",
              reasonCode: "coordinator_restarting",
              message: "Waiting for coordinator recovery",
              nextAttemptAtMs: this.#now(),
            }, "Coordinator restarted; recovery is pending", this.#now()));
      }
    }
  }
  readSummary(workerFp: string): WorkerUpdateOperation | null {
    return this.#latestByWorker.get(workerFp) ?? null;
  }
  readReport(workerFp: string): WorkerUpdateReport | null {
    const operation = this.#latestByWorker.get(workerFp);
    return operation ? this.#jobs.get(operation.jobId)?.record.report ?? null : null;
  }
  ownsJob(jobId: string): boolean {
    return this.#jobs.has(jobId);
  }
  async startDeploy(request: WorkerUpdateStartRequest): Promise<DeployStartResult> {
    const validated = validateWorkerUpdateStart(request);
    if (!validated.ok) return validated;
    if (!this.#accepting) return { ok: false, error: "coordinator is shutting down" };
    if (this.#corruptWorkers.has(request.workerFp)) {
      return { ok: false, error: "worker update record is corrupt; retained for diagnosis" };
    }
    const normalizedHost = request.host.toLowerCase();
    const existingWorkerJobId = this.#unresolvedJobByWorker.get(request.workerFp);
    const existingHostJobId = this.#unresolvedJobByHost.get(normalizedHost);
    const existingJobId = existingWorkerJobId ?? existingHostJobId;
    if (existingJobId) {
      const existing = this.#jobs.get(existingJobId);
      if (existing?.record.operation.workerFp !== request.workerFp) {
        return { ok: false, error: "deploy host is owned by another worker identity" };
      }
      if (existing.record.operation.targetGitSha === request.expectedGitSha) {
        if (request.source === "manual" && existing.record.operation.status === "waiting") {
          this.#enqueue(existingJobId);
        }
        return { ok: true, jobId: existingJobId };
      }
      return { ok: false, jobId: existingJobId, error: "another target owns this worker update" };
    }
    const latest = this.#latestByWorker.get(request.workerFp);
    const jobId = crypto.randomUUID();
    const now = this.#now();
    const operation = initialWorkerUpdateOperation({
      request,
      jobId,
      revision: (latest?.revision ?? 0) + 1,
      atMs: now,
    });
    this.#claimUnresolved(operation);
    try {
      const canonicalSourceRoot = await canonicalCoordinatorSourceRoot(request.sourceRoot);
      if (!await this.#deps.workerExists(request.workerFp)) {
        this.#releaseUnresolved(operation);
        return { ok: false, error: "worker not found" };
      }
      const baseline = await this.#deps.readBaseline(request.workerFp);
      const record = initialDeployJobRecord({
        operation,
        sourceRoot: canonicalSourceRoot,
        baseline,
        coordinatorOrigin: this.#deps.coordinatorOrigin,
      });
      const job = this.#installRecord(record);
      await this.#mutate(job, record);
      if (request.source !== "manual" && !this.#deps.workerRoutable(request.workerFp)) {
        await this.#mutate(job, offlineDeployJobRecord(job.record, this.#now()));
        return { ok: true, jobId };
      }
      this.#enqueue(jobId);
      return { ok: true, jobId };
    } catch (error) {
      this.#releaseUnresolved(operation);
      return { ok: false, error: error instanceof Error ? error.message : String(error) };
    }
  }
  async *output(jobId: string, signal?: AbortSignal): AsyncGenerator<DeployStreamMsg> {
    let job = this.#jobs.get(jobId);
    if (!job) {
      const operation = [...this.#latestByWorker.values()].find(candidate => candidate.jobId === jobId);
      if (operation) {
        const loaded = await loadDeployJobRecord(operation.workerFp, jobId);
        if (loaded.kind === "record") job = this.#installRecord(loaded.record);
      }
    }
    if (!job) return;
    yield { kind: "operation", operation: job.record.operation };
    yield { kind: "report", report: job.record.report };
    for (const text of job.record.lines) yield { kind: "line", text };
    if (workerUpdateStatusIsTerminal(job.record.operation.status)) {
      yield terminalDeployFrame(job.record.operation);
      return;
    }
    for await (const message of busToAsyncIterable(job.bus, { signal })) {
      yield message;
      if (message.kind === "done") return;
    }
  }
  async sweep(): Promise<void> {
    const now = this.#now();
    for (const [jobId, job] of this.#jobs) {
      const operation = job.record.operation;
      if (operation.status !== "waiting" || job.runtime) continue;
      if ((operation.nextAttemptAtMs ?? 0) > now) continue;
      if (!await this.#deps.workerExists(operation.workerFp)) continue;
      this.#enqueue(jobId);
    }
  }
  async deleteWorker(workerFp: string): Promise<void> {
    this.#queue.splice(0, this.#queue.length, ...this.#queue.filter(jobId =>
      this.#jobs.get(jobId)?.record.operation.workerFp !== workerFp));
    const jobId = this.#unresolvedJobByWorker.get(workerFp);
    const job = jobId ? this.#jobs.get(jobId) : undefined;
    if (job?.runtime) await job.runtime.result;
    if (job) this.#releaseUnresolved(job.record.operation);
    this.#latestByWorker.delete(workerFp);
  }
  async dispose(): Promise<void> {
    this.#accepting = false;
    this.#queue.length = 0;
    const running = [...this.#jobs.values()].filter(job => job.runtime);
    for (const job of running) job.runtime?.stop();
    await Promise.all(running.map(job => job.runtime?.result));
    await Promise.all(this.#executions);
  }
  #installRecord(record: PersistedDeployJob): LiveDeployJob {
    const existing = this.#jobs.get(record.operation.jobId);
    if (existing) {
      if (record.operation.revision >= existing.record.operation.revision) existing.record = record;
      return existing;
    }
    const job: LiveDeployJob = {
      record,
      bus: new BoundedBus<DeployStreamMsg>(2_048),
      runtime: null,
      mutationTail: Promise.resolve(),
    };
    this.#jobs.set(record.operation.jobId, job);
    return job;
  }
  #claimUnresolved(operation: WorkerUpdateOperation): void {
    this.#unresolvedJobByWorker.set(operation.workerFp, operation.jobId);
    this.#unresolvedJobByHost.set(operation.host.toLowerCase(), operation.jobId);
  }
  #releaseUnresolved(operation: WorkerUpdateOperation): void {
    if (this.#unresolvedJobByWorker.get(operation.workerFp) === operation.jobId) {
      this.#unresolvedJobByWorker.delete(operation.workerFp);
    }
    if (this.#unresolvedJobByHost.get(operation.host.toLowerCase()) === operation.jobId) {
      this.#unresolvedJobByHost.delete(operation.host.toLowerCase());
    }
  }
  #enqueue(jobId: string): void {
    if (!this.#queue.includes(jobId)) this.#queue.push(jobId);
    void this.#drain();
  }
  async #drain(): Promise<void> {
    while (this.#activeCount < MAX_ACTIVE_POSIX_JOBS && this.#queue.length > 0) {
      const jobId = this.#queue.shift()!;
      const job = this.#jobs.get(jobId);
      if (!job || job.runtime || workerUpdateStatusIsTerminal(job.record.operation.status)) continue;
      this.#activeCount += 1;
      const execution = this.#execute(job).finally(() => {
        this.#activeCount -= 1;
        this.#executions.delete(execution);
        void this.#drain();
      });
      this.#executions.add(execution);
      void execution;
    }
  }
  async #execute(job: LiveDeployJob): Promise<void> {
    const startedAtMs = job.record.operation.startedAtMs ?? this.#now();
    await this.#mutate(job, nextDeployJobRecord(job.record, {
      status: "running",
      phase: job.record.operation.phase === "recovery" ? "recovery" : "staging",
      reasonCode: null,
      message: "Worker update running",
      startedAtMs,
      nextAttemptAtMs: null,
    }, "Worker update started", this.#now()));
    const request: WorkerUpdateStartRequest = {
      workerFp: job.record.operation.workerFp,
      host: job.record.operation.host,
      expectedGitSha: job.record.operation.targetGitSha,
      source: job.record.operation.source,
      sourceRoot: job.record.sourceRoot,
      sourceMode: "coordinator-pinned",
    };
    const runtime = (this.#deps.startRuntime ?? startDeployJobRuntime)(
      request,
      job.record.operation.jobId,
      this.#deps.coordinatorDialUrl,
      {
        onLine: line => this.#mutate(job, {
          ...job.record,
          lines: appendDeployOutputLine(job.record.lines, line),
        }),
        onProgress: event => this.#recordProgress(job, event),
      },
    );
    job.runtime = runtime;
    const result = await runtime.result;
    job.runtime = null;
    if (!this.#accepting) { await this.#mutate(job, interruptedDeployJobRecord(job.record, this.#now())); return; }
    if (result.timedOut) {
      await this.#mutate(job, nextDeployJobRecord(job.record, {
        status: "waiting",
        phase: "recovery",
        reasonCode: "confirmation_pending",
        message: "Waiting for deployment recovery",
        nextAttemptAtMs: this.#now() + WORKER_UPDATE_RETRY_DELAY_MS,
        exitCode: result.exitCode,
      }, "Deployment subprocess ended; recovery remains pending", this.#now()));
      return;
    }
    if (result.error || result.exitCode !== 0) {
      const classified = classifyWorkerUpdateFailure(result.failure);
      await this.#finish(
        job,
        classified.status,
        classified.reasonCode,
        result.failure?.message ?? "Worker deployment failed",
        result.exitCode,
        null,
        result.failure,
      );
      return;
    }
    if (!result.settlementProven) {
      await this.#mutate(job, nextDeployJobRecord(job.record, {
        status: "waiting",
        phase: "recovery",
        reasonCode: "journal_conflict",
        message: "Waiting for host journal settlement",
        nextAttemptAtMs: this.#now() + WORKER_UPDATE_RETRY_DELAY_MS,
        exitCode: 0,
      }, "Host journal settlement remains pending", this.#now()));
      return;
    }
    await this.#mutate(job, nextDeployJobRecord(job.record, {
      status: "verifying",
      phase: "confirmation",
      reasonCode: "confirmation_pending",
      message: "Waiting for worker confirmation",
      exitCode: 0,
    }, "Waiting for worker confirmation", this.#now()));
    await this.#verify(job);
  }
  async #verify(job: LiveDeployJob): Promise<void> {
    const deadline = this.#now() + WORKER_UPDATE_VERIFY_TIMEOUT_MS;
    while (this.#now() <= deadline) {
      if (!this.#accepting) { await this.#mutate(job, interruptedDeployJobRecord(job.record, this.#now())); return; }
      const proof = await this.#deps.readVerification(job.record.operation.workerFp);
      if (proof?.routable
        && proof.heartbeatAtMs > job.record.baseline.heartbeatAtMs
        && proof.gitSha === job.record.operation.targetGitSha
        && proof.journalSettled
        && proof.keeperConverged) {
        await this.#finish(job, "succeeded", null, "Worker update succeeded", 0, proof.gitSha);
        return;
      }
      await (this.#deps.sleep ?? Bun.sleep)(WORKER_UPDATE_VERIFY_POLL_MS);
    }
    if (!this.#accepting) { await this.#mutate(job, interruptedDeployJobRecord(job.record, this.#now())); return; }
    await this.#finish(
      job,
      "failed",
      "confirmation_timeout",
      "Worker confirmation timed out",
      0,
    );
  }
  async #finish(
    job: LiveDeployJob,
    status: "succeeded" | "failed" | "blocked",
    reasonCode: WorkerUpdateOperation["reasonCode"],
    message: string,
    exitCode: number | null,
    observedGitSha: string | null = null,
    failureOverride: WorkerUpdateFailure | null = null,
  ): Promise<void> {
    const record = finishedDeployJobRecord({
      record: job.record,
      status,
      reasonCode,
      message,
      exitCode,
      atMs: this.#now(),
      observedGitSha,
      failureOverride,
    });
    await this.#mutate(job, record);
    this.#releaseUnresolved(record.operation);
    job.bus.publish(terminalDeployFrame(record.operation));
  }
  async #recordProgress(job: LiveDeployJob, event: WorkerUpdateProgressEvent): Promise<void> {
    await this.#mutate(job, nextDeployJobRecord(job.record, {
      phase: event.phase,
      message: event.message,
    }, event.message, this.#now()));
  }
  async #mutate(job: LiveDeployJob, record: PersistedDeployJob): Promise<void> {
    job.mutationTail = job.mutationTail.then(async () => {
      await persistDeployJobRecord(record);
      job.record = record;
      const current = this.#latestByWorker.get(record.operation.workerFp);
      if (!current || record.operation.revision > current.revision) {
        this.#latestByWorker.set(record.operation.workerFp, record.operation);
        job.bus.publish({ kind: "operation", operation: record.operation });
        job.bus.publish({ kind: "report", report: record.report });
        await this.#deps.publishOperation(record.operation.workerFp, record.operation);
      }
    });
    try {
      await job.mutationTail;
    } catch (error) {
      log.error("deploy", "job_record_persist_failed", {
        job_id: record.operation.jobId,
        worker_fp: record.operation.workerFp,
        error: String(error),
      });
      throw error;
    }
  }
  #now(): number { return (this.#deps.now ?? Date.now)(); }
}
