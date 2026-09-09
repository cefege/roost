// Owns bounded, target-scoped terminal pipeline sampling for one system handler.
// Concurrent requests batch only uncached targets for the same authenticated worker.
// Cached records never cross target scopes; a generation replacement starts fresh.
// Wire validation and direct worker RPC framing remain in worker-terminal-pipeline-snapshot.ts.

import { create } from "@bufbuild/protobuf";
import {
  WTerminalPipelineSnapshotSchema,
} from "@roost/shared/proto/worker_transport_pb";
import type { TerminalPipelineSessionSnapshot } from "@roost/shared/proto/wire_pb";
import type { WorkerHandle } from "./worker-registry.ts";
import { currentRoutableWorker } from "./worker-send-target.ts";
import {
  TERMINAL_PIPELINE_DIAG_MAX_TARGETS,
  collectWorkerTerminalPipelineSnapshots,
  normalizeTerminalPipelineDiagnosticTargets,
  type TerminalPipelineDiagnosticTarget,
  type WorkerTerminalPipelineSnapshotResult,
} from "./worker-terminal-pipeline-snapshot.ts";

export const WORKER_TERMINAL_PIPELINE_CACHE_MS = 2_000;

export interface WorkerTerminalPipelineSnapshotCache {
  collect(
    targetsByWorker: ReadonlyMap<string, readonly TerminalPipelineDiagnosticTarget[]>,
    timeoutMs?: number,
  ): Promise<Record<string, WorkerTerminalPipelineSnapshotResult>>;
  dispose(): void;
}

interface CachedTargetSample {
  expiresAtMs: number;
  requestId: string;
  responseMs: number;
  session: TerminalPipelineSessionSnapshot | null;
}

interface PendingTargetCollection {
  readonly resolve: (result: WorkerTerminalPipelineSnapshotResult) => void;
  readonly targets: readonly TerminalPipelineDiagnosticTarget[];
}

interface WorkerPipelineCacheEntry {
  cachedError: WorkerTerminalPipelineSnapshotResult | null;
  errorExpiresAtMs: number;
  expiryTimer: Timer | null;
  flushScheduled: boolean;
  inFlight: boolean;
  nextRequestAtMs: number;
  pending: PendingTargetCollection[];
  requestTimer: Timer | null;
  targets: Map<string, CachedTargetSample>;
  worker: WorkerHandle;
}

/** Creates explicit handler-owned cache state; no process-global diagnostic cache exists. */
export function createWorkerTerminalPipelineSnapshotCache(): WorkerTerminalPipelineSnapshotCache {
  const entries = new Map<string, WorkerPipelineCacheEntry>();

  const pipelineError = (message: string): WorkerTerminalPipelineSnapshotResult => ({
    status: "error",
    response_ms: 0,
    error: { code: "offline", message },
  });
  const settlePending = (
    entry: WorkerPipelineCacheEntry,
    result: WorkerTerminalPipelineSnapshotResult,
  ): void => {
    const pending = entry.pending.splice(0);
    for (const collection of pending) collection.resolve(result);
  };
  const clearEntry = (workerFp: string, entry: WorkerPipelineCacheEntry, message: string): void => {
    clearTimeout(entry.expiryTimer ?? undefined);
    clearTimeout(entry.requestTimer ?? undefined);
    entry.expiryTimer = null;
    entry.requestTimer = null;
    entry.flushScheduled = false;
    settlePending(entry, pipelineError(message));
    if (entries.get(workerFp) === entry) entries.delete(workerFp);
  };
  const cacheTarget = (
    entry: WorkerPipelineCacheEntry,
    target: TerminalPipelineDiagnosticTarget,
  ): CachedTargetSample | null => {
    const key = targetKey(target);
    const sample = entry.targets.get(key);
    if (!sample) return null;
    if (sample.expiresAtMs > Date.now()) return sample;
    entry.targets.delete(key);
    return null;
  };
  const projectCachedTargets = (
    entry: WorkerPipelineCacheEntry,
    targets: readonly TerminalPipelineDiagnosticTarget[],
  ): WorkerTerminalPipelineSnapshotResult | null => {
    if (entry.cachedError !== null && entry.errorExpiresAtMs > Date.now()) return entry.cachedError;
    const samples = targets.map((target) => cacheTarget(entry, target));
    if (samples.some((sample) => sample === null)) return null;
    const firstSample = samples[0]!;
    const sessions = samples.flatMap((sample) => sample?.session === null ? [] : [sample!.session]);
    return {
      status: "ok",
      response_ms: firstSample!.responseMs,
      snapshot: create(WTerminalPipelineSnapshotSchema, {
        requestId: firstSample!.requestId,
        droppedTargets: 0,
        droppedRecords: targets.length - sessions.length,
        sessions,
      }),
    };
  };
  const resolveSatisfiedPending = (entry: WorkerPipelineCacheEntry): void => {
    for (let index = entry.pending.length - 1; index >= 0; index--) {
      const collection = entry.pending[index]!;
      const result = projectCachedTargets(entry, collection.targets);
      if (result === null) continue;
      entry.pending.splice(index, 1);
      collection.resolve(result);
    }
  };
  const armExpiry = (workerFp: string, entry: WorkerPipelineCacheEntry): void => {
    clearTimeout(entry.expiryTimer ?? undefined);
    const deadlines = [entry.errorExpiresAtMs, ...[...entry.targets.values()].map((sample) => sample.expiresAtMs)]
      .filter((deadline) => deadline > Date.now());
    if (deadlines.length === 0) {
      if (!entry.inFlight && entry.pending.length === 0) clearEntry(workerFp, entry, "pipeline cache expired");
      return;
    }
    const deadline = Math.min(...deadlines);
    entry.expiryTimer = setTimeout(() => {
      entry.expiryTimer = null;
      if (entries.get(workerFp) !== entry) return;
      for (const [key, sample] of entry.targets) {
        if (sample.expiresAtMs <= Date.now()) entry.targets.delete(key);
      }
      if (entry.errorExpiresAtMs <= Date.now()) entry.cachedError = null;
      if (!entry.inFlight && entry.pending.length === 0 && entry.targets.size === 0 && entry.cachedError === null) {
        clearEntry(workerFp, entry, "pipeline cache expired");
        return;
      }
      armExpiry(workerFp, entry);
    }, Math.max(0, deadline - Date.now()));
    entry.expiryTimer.unref?.();
  };
  const scheduleBatch = (
    workerFp: string,
    entry: WorkerPipelineCacheEntry,
    timeoutMs: number | undefined,
  ): void => {
    if (entries.get(workerFp) !== entry || entry.inFlight || entry.pending.length === 0) return;
    const remainingDelayMs = entry.nextRequestAtMs - Date.now();
    if (remainingDelayMs > 0) {
      if (entry.requestTimer !== null) return;
      entry.requestTimer = setTimeout(() => {
        entry.requestTimer = null;
        scheduleBatch(workerFp, entry, timeoutMs);
      }, remainingDelayMs);
      entry.requestTimer.unref?.();
      return;
    }
    if (entry.flushScheduled) return;
    entry.flushScheduled = true;
    queueMicrotask(() => {
      entry.flushScheduled = false;
      if (entries.get(workerFp) !== entry || entry.inFlight) return;
      const targets = new Map<string, TerminalPipelineDiagnosticTarget>();
      for (const collection of entry.pending) {
        for (const target of collection.targets) {
          if (cacheTarget(entry, target) !== null || targets.has(targetKey(target))) continue;
          if (targets.size === TERMINAL_PIPELINE_DIAG_MAX_TARGETS) break;
          targets.set(targetKey(target), target);
        }
        if (targets.size === TERMINAL_PIPELINE_DIAG_MAX_TARGETS) break;
      }
      resolveSatisfiedPending(entry);
      if (targets.size === 0) {
        if (entry.pending.length > 0) scheduleBatch(workerFp, entry, timeoutMs);
        return;
      }
      entry.inFlight = true;
      void collectWorkerTerminalPipelineSnapshots(
        new Map([[workerFp, [...targets.values()]]]),
        timeoutMs,
      ).then((results) => {
        if (entries.get(workerFp) !== entry) return;
        entry.inFlight = false;
        const result = results[workerFp] ?? pipelineError("worker pipeline request was not collected");
        entry.nextRequestAtMs = Date.now() + WORKER_TERMINAL_PIPELINE_CACHE_MS;
        if (result.status !== "ok") {
          entry.cachedError = result;
          entry.errorExpiresAtMs = entry.nextRequestAtMs;
          settlePending(entry, result);
          armExpiry(workerFp, entry);
          return;
        }
        const sessionsByTarget = new Map(result.snapshot.sessions.map((session) => [
          targetKey({ sessionId: session.sessionId, viewId: session.viewId }),
          session,
        ]));
        for (const target of targets.values()) {
          entry.targets.set(targetKey(target), {
            session: sessionsByTarget.get(targetKey(target)) ?? null,
            requestId: result.snapshot.requestId,
            responseMs: result.response_ms,
            expiresAtMs: entry.nextRequestAtMs,
          });
        }
        resolveSatisfiedPending(entry);
        armExpiry(workerFp, entry);
        scheduleBatch(workerFp, entry, timeoutMs);
      }, () => {
        if (entries.get(workerFp) !== entry) return;
        entry.inFlight = false;
        settlePending(entry, pipelineError("worker pipeline request failed"));
        clearEntry(workerFp, entry, "worker pipeline request failed");
      });
    });
  };
  const collectWorker = (
    workerFp: string,
    targets: readonly TerminalPipelineDiagnosticTarget[],
    timeoutMs: number | undefined,
  ): Promise<WorkerTerminalPipelineSnapshotResult> => {
    const normalizedTargets = normalizeTerminalPipelineDiagnosticTargets(targets);
    if (normalizedTargets.length === 0) return Promise.resolve(pipelineError("no valid terminal pipeline targets"));
    const worker = currentRoutableWorker(workerFp);
    if (!worker) {
      return Promise.resolve(pipelineError("worker is not connected"));
    }
    let entry = entries.get(workerFp);
    if (entry && entry.worker !== worker) {
      clearEntry(workerFp, entry, "worker connection changed");
      entry = undefined;
    }
    if (!entry) {
      entry = {
        worker,
        cachedError: null,
        errorExpiresAtMs: 0,
        expiryTimer: null,
        flushScheduled: false,
        inFlight: false,
        nextRequestAtMs: 0,
        pending: [],
        requestTimer: null,
        targets: new Map(),
      };
      entries.set(workerFp, entry);
    }
    const cached = projectCachedTargets(entry, normalizedTargets);
    if (cached !== null) return Promise.resolve(cached);
    const pendingResult = Promise.withResolvers<WorkerTerminalPipelineSnapshotResult>();
    entry!.pending.push({ resolve: pendingResult.resolve, targets: normalizedTargets });
    scheduleBatch(workerFp, entry!, timeoutMs);
    return pendingResult.promise;
  };

  return {
    async collect(targetsByWorker, timeoutMs) {
      const requests = [...targetsByWorker]
        .map(([workerFp, targets]) => [workerFp, normalizeTerminalPipelineDiagnosticTargets(targets)] as const)
        .filter(([, targets]) => targets.length !== 0)
        .sort(([left], [right]) => left.localeCompare(right));
      const settled = await Promise.allSettled(requests.map(([workerFp, targets]) =>
        collectWorker(workerFp, targets, timeoutMs)));
      return Object.fromEntries(requests.map(([workerFp], index) => [
        workerFp,
        settled[index]?.status === "fulfilled"
          ? settled[index].value
          : pipelineError("terminal pipeline collection failed"),
      ]));
    },
    dispose() {
      for (const [workerFp, entry] of entries) clearEntry(workerFp, entry, "pipeline cache disposed");
    },
  };
}

function targetKey(target: TerminalPipelineDiagnosticTarget): string {
  return JSON.stringify([target.sessionId, target.viewId]);
}
