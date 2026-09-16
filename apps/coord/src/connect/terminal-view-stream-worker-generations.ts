// Worker connection-generation ledger for TerminalViewStreamController.
// A "never" verdict means a worker answered outside the stream protocol, so
// its route is never retried against that worker again. A DIFFERENT worker
// connection is a participant that verdict never tested, and it is the only
// event allowed to clear one — no timer, no heartbeat, no same-connection
// reconcile. Generation identity is the routable handle the stream dispatcher
// already fences on, so a seam that exposes no handle keeps every verdict
// fail-closed rather than guessing that the worker changed.
import { log } from "@roost/shared/log";
import type { TerminalStreamState } from "@roost/shared/terminal-view";

/** One session's stream state plus the worker generation its current
 * unavailable verdict was formed under. */
export interface TerminalStreamVerdictState extends TerminalStreamState {
  unavailableGeneration: number;
}

/** What a route reconcile may do with one session: leave a fail-closed verdict
 * latched, clear it for a newer worker connection, or reconcile normally. */
export type TerminalVerdictReconcileDecision = "skip" | "clear" | "proceed";

export class TerminalWorkerGenerations {
  private readonly observed = new Map<string, { handle: unknown; generation: number }>();
  private latestGeneration = 0;

  constructor(private readonly currentWorker: (workerFp: string) => unknown | null) {}

  /** The generation any verdict formed right now belongs to. */
  currentGeneration(): number {
    return this.latestGeneration;
  }

  /** Counts a worker replacement only when a different routable handle is
   * behind it: one connection re-announcing its own fleet snapshot repeats the
   * notice without being a new participant. */
  observeReplacement(workerFp: string): void {
    const handle = this.currentWorker(workerFp);
    // No handle means no observable participant: a seam that cannot name the
    // worker connection never earns a generation, so verdicts stay fail-closed.
    if (handle == null) return;
    const previous = this.observed.get(workerFp);
    if (previous?.handle === handle) return;
    this.latestGeneration += 1;
    this.observed.set(workerFp, { handle, generation: this.latestGeneration });
    log.info("terminal-view-stream", "worker_generation_observed", {
      worker_fp: workerFp,
      previous_generation: previous?.generation ?? 0,
      generation: this.latestGeneration,
    });
  }

  /** A retired fingerprint keeps no handle record; generations never rewind,
   * so a later re-enrollment still reads as strictly newer. */
  forgetWorker(workerFp: string): void {
    this.observed.delete(workerFp);
  }

  reconcileDecision(
    workerFp: string,
    session: TerminalStreamVerdictState,
  ): TerminalVerdictReconcileDecision {
    if (!session.unavailable || session.unavailablePolicy !== "never") return "proceed";
    const generation = this.observed.get(workerFp)?.generation ?? 0;
    return generation > session.unavailableGeneration ? "clear" : "skip";
  }

  /** A fail-closed pane coming back must never be silent: one line names the
   * generation that earned the verdict and the one clearing it. */
  noteCleared(
    sessionId: string,
    workerFp: string,
    session: TerminalStreamVerdictState,
  ): void {
    log.info("terminal-view-stream", "stream_invariant_verdict_cleared", {
      session_id: sessionId,
      worker_fp: workerFp,
      verdict_generation: session.unavailableGeneration,
      worker_generation: this.observed.get(workerFp)?.generation ?? 0,
      reason: session.unavailableReason,
    });
  }
}
