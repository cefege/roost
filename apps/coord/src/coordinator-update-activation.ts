// Process-owned adapter between coordinator deploy journal V4 and the single
// CoordinatorWriteGate. A matching target boot holds durable mutation ingress
// until the CLI checkpoints finalizing; malformed evidence never releases it.

import { readFile } from "node:fs/promises";
import {
  CoordinatorDeployJournalV4Schema,
  type CoordinatorDeployJournalV4,
} from "@roost/shared/coordinator-deploy-state";
import { log } from "@roost/shared/log";
import type { CoordinatorWriteGate, WriteLease } from "./coordinator-write-gate.ts";

const ACTIVATION_RECHECK_MS = 250;

export interface CoordinatorActivationGate {
  updateReady(): boolean;
  updateTransactionId(): string | null;
  dispose(): void;
}

export async function createCoordinatorActivationGate(options: {
  journalPath: string;
  sourceRoot: string;
  targetSha: string;
  writeGate: CoordinatorWriteGate;
  scheduleRecheck?: (callback: () => Promise<void>) => () => void;
}): Promise<CoordinatorActivationGate> {
  let disposed = false;
  let lease: WriteLease | null = null;
  let transactionId: string | null = null;
  let ready = true;
  let cancelRecheck: (() => void) | null = null;
  const initial = await readValidatedJournal(options.journalPath);
  if (initial && bootMatchesTarget(initial, options.sourceRoot, options.targetSha)) {
    transactionId = initial.rolloutId;
    ready = initial.phase === "finalizing";
    if (!ready) {
      lease = await options.writeGate.acquireExclusive(
        `coordinator-activation:${initial.rolloutId}`,
      );
      cancelRecheck = (options.scheduleRecheck ?? scheduleActivationRecheck)(
        reevaluate,
      );
      log.info("coord-update", "activation_gate_held", {
        rollout_id: initial.rolloutId,
        phase: initial.phase,
      });
    }
  }

  async function reevaluate(): Promise<void> {
    if (disposed || ready || transactionId === null) return;
    let journal: CoordinatorDeployJournalV4 | null;
    try {
      journal = await readValidatedJournal(options.journalPath);
    } catch (error) {
      log.warn("coord-update", "activation_evidence_invalid", {
        rollout_id: transactionId,
        error: String(error),
      });
      return;
    }
    if (!journal
      || journal.rolloutId !== transactionId
      || !bootMatchesTarget(journal, options.sourceRoot, options.targetSha)
      || journal.phase !== "finalizing") return;
    ready = true;
    cancelRecheck?.();
    cancelRecheck = null;
    lease?.release();
    lease = null;
    log.info("coord-update", "activation_gate_released", {
      rollout_id: transactionId,
    });
  }

  return {
    updateReady: () => ready,
    updateTransactionId: () => transactionId,
    dispose: () => {
      if (disposed) return;
      disposed = true;
      cancelRecheck?.();
      cancelRecheck = null;
      lease?.release();
      lease = null;
    },
  };
}

async function readValidatedJournal(
  journalPath: string,
): Promise<CoordinatorDeployJournalV4 | null> {
  let serialized: string;
  try {
    serialized = await readFile(journalPath, "utf8");
  } catch (error) {
    const code = error instanceof Error && "code" in error
      ? (error as NodeJS.ErrnoException).code
      : undefined;
    if (code === "ENOENT") return null;
    throw error;
  }
  if (Buffer.byteLength(serialized) > 2 * 1024 * 1024) {
    throw new Error("coordinator activation journal exceeds the size limit");
  }
  return CoordinatorDeployJournalV4Schema.parse(JSON.parse(serialized));
}

function bootMatchesTarget(
  journal: CoordinatorDeployJournalV4,
  sourceRoot: string,
  targetSha: string,
): boolean {
  return journal.stagedReleasePath === sourceRoot && journal.targetSha === targetSha;
}

function scheduleActivationRecheck(callback: () => Promise<void>): () => void {
  const timer = setInterval(() => void callback(), ACTIVATION_RECHECK_MS);
  timer.unref();
  return () => clearInterval(timer);
}
