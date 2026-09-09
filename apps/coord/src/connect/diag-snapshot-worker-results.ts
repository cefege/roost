// Assembles generic and typed worker evidence for DiagSnapshot.
// The handler supplies durable open-session rows and current routable workers.
// This owner never expands a worker response beyond those authorized session IDs.
// Typed pipeline evidence stays content-free and has one bounded request per worker.

import {
  collectWorkerDiagSnapshots,
  type WorkerDiagSnapshotResult,
} from "./worker-diag-snapshot.ts";
import {
  terminalPipelineDiagnosticSnapshot,
  type WorkerTerminalPipelineSnapshotResult,
} from "./worker-terminal-pipeline-snapshot.ts";
import { createWorkerTerminalPipelineSnapshotCache } from "./worker-terminal-pipeline-cache.ts";

interface ScopedDiagnosticSession {
  id: string;
  worker_fp: string;
}

interface ScopedWorkerDiagnosticOptions {
  workerFps: ReadonlySet<string>;
  sessions: readonly ScopedDiagnosticSession[];
  allowedSessionIds: ReadonlySet<string>;
}

/** Creates the handler-owned collector for durable session-scoped worker evidence. */
export function createScopedWorkerDiagnosticCollector() {
  const pipelineSnapshots = createWorkerTerminalPipelineSnapshotCache();
  return async function collectScopedWorkerDiagnostics(
    options: ScopedWorkerDiagnosticOptions,
  ) {
    const pipelineTargetsByWorker = new Map<string, Array<{
      sessionId: string;
      viewId: string;
    }>>();
    for (const session of options.sessions) {
      if (!options.workerFps.has(session.worker_fp)) continue;
      const targets = pipelineTargetsByWorker.get(session.worker_fp) ?? [];
      // This is a coordinator diagnostic target, not a browser view claim.
      targets.push({ sessionId: session.id, viewId: "" });
      pipelineTargetsByWorker.set(session.worker_fp, targets);
    }

    const [workerSnapshots, pipelineResults] = await Promise.all([
      collectWorkerDiagSnapshots(options.workerFps),
      pipelineSnapshots.collect(pipelineTargetsByWorker),
    ]);
    return Object.fromEntries(
      Object.entries(workerSnapshots).map(([workerFp, result]) => {
        const workerDiagnostic = scopedWorkerDiagnostic(
          workerFp,
          result,
          options.allowedSessionIds,
        );
        const pipelineDiagnostic = pipelineResults[workerFp];
        return [
          workerFp,
          pipelineDiagnostic === undefined
            ? workerDiagnostic
            : {
              ...workerDiagnostic,
              terminal_pipeline: scopedWorkerTerminalPipelineDiagnostic(
                pipelineDiagnostic,
                options.allowedSessionIds,
              ),
            },
        ] as const;
      }),
    );
  };
}

function scopedWorkerDiagnostic(
  workerFp: string,
  result: WorkerDiagSnapshotResult,
  allowedSessionIds: ReadonlySet<string>,
): WorkerDiagSnapshotResult {
  if (result.status !== "ok") return result;
  const sourceSessions = result.snapshot.sessions;
  const sessions = sourceSessions !== null
    && typeof sourceSessions === "object"
    && !Array.isArray(sourceSessions)
    ? Object.fromEntries(
      Object.entries(sourceSessions)
        .filter(([sessionId]) => allowedSessionIds.has(sessionId)),
    )
    : {};
  return {
    ...result,
    snapshot: {
      captured_at_ms: result.snapshot.captured_at_ms,
      build: result.snapshot.build,
      worker_fp: workerFp,
      sessions,
    },
  };
}

function scopedWorkerTerminalPipelineDiagnostic(
  result: WorkerTerminalPipelineSnapshotResult,
  allowedSessionIds: ReadonlySet<string>,
) {
  if (result.status !== "ok") return result;
  return {
    ...result,
    snapshot: terminalPipelineDiagnosticSnapshot(result.snapshot, allowedSessionIds),
  };
}
