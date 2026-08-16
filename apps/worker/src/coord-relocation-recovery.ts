import { CoordinatorMovePhase } from "@roost/shared/proto/coordinator_pb";
import type { CoordRelocationJournal, WorkerCoordRelocation } from "./coord-relocation.ts";
import type { CoordLink } from "./transport/CoordLink-types.ts";


const TARGET_OWNED_PHASES = new Set<CoordinatorMovePhase>([
  CoordinatorMovePhase.WAITING_FOR_WORKERS,
  CoordinatorMovePhase.COMMITTING,
  CoordinatorMovePhase.COMMITTED,
]);
export type MoveStatus = { phase: CoordinatorMovePhase };

export interface CoordRelocationRecoveryOptions {
  relocation: WorkerCoordRelocation;
  link: CoordLink;
  statusAt: (url: string, handoffId: string) => Promise<MoveStatus>;
  setCoordinatorEndpoint: (url: string) => void;
  /** The failover cutover below is the lossy path — the source died, so no
   *  COMMIT frame ever arrives to trigger main.ts's own re-announce and the
   *  new coordinator would have no session projection. */
  reannounce: (targetUrl: string) => void;
  now?: () => number;
  abortTarget: (handoffId: string) => Promise<void>;
  unavailableAfterMs?: number;
  /** Lets the GC pass tell a spent COMMITTED journal from a live one. */
  currentCoordinatorUrl?: () => string;
}

/**
 * Resolves a worker's staged move only when its current coordinator has been
 * unavailable long enough to be a failed cutover rather than a transient dial.
 * The target's status is authenticated independently so a stale local journal
 * never chooses an endpoint on its own.
 */
export function createCoordRelocationRecovery(options: CoordRelocationRecoveryOptions): () => Promise<void> {
  const now = options.now ?? Date.now;
  const unavailableAfterMs = options.unavailableAfterMs ?? 15_000;
  let unavailableSinceMs: number | null = null;
  let running = false;
  let gcInFlight = false;

  return async (): Promise<void> => {
    if (running) return;
    const journal = options.relocation.load();
    if (!journal) {
      unavailableSinceMs = null;
      return;
    }
    if (options.link.state().kind === "open") {
      unavailableSinceMs = null;
      // The link is healthy, so nothing needs recovering — but a journal that
      // outlived its move must not survive to arm a future failover.
      if (!gcInFlight) {
        gcInFlight = true;
        void collectStaleJournal(options, journal).finally(() => { gcInFlight = false; });
      }
      return;
    }
    // commit() keeps the journal so a service restart still finds the new
    // endpoint. It is settled; re-running recovery against it would probe the
    // retired source every tick and could downgrade it back to ACTIVATED.
    if (journal.state === "COMMITTED") {
      unavailableSinceMs = null;
      return;
    }
    const currentMs = now();
    unavailableSinceMs ??= currentMs;
    if (currentMs - unavailableSinceMs < unavailableAfterMs) return;

    running = true;
    try {
      if (!await resolveUnreachableSource(options, journal)) unavailableSinceMs = null;
    } finally {
      running = false;
    }
  };
}

/** A STAGED record from a move rolled back while this worker was offline (the
 *  ABORT is swallowed by the orchestrator's Promise.allSettled) otherwise
 *  survives forever. A COMMITTED record is spent once we are already talking to
 *  its target. */
async function collectStaleJournal(
  options: CoordRelocationRecoveryOptions,
  journal: CoordRelocationJournal,
): Promise<void> {
  if (journal.state === "COMMITTED") {
    if (options.currentCoordinatorUrl?.() === journal.target_url) await options.relocation.discard();
    return;
  }
  if (journal.state === "ACTIVATED") {
    // The durable rewrite is deliberately deferred to the target's COMMIT, so
    // an ACTIVATED journal outlives a target that rolled back afterwards. With
    // the link open to that target and the source unreachable, this GC pass is
    // the only thing left that can re-poll and unstick the worker.
    let targetPhase: CoordinatorMovePhase;
    try {
      targetPhase = (await options.statusAt(journal.target_url, journal.handoff_id)).phase;
    } catch {
      return;
    }
    if (targetPhase !== CoordinatorMovePhase.ROLLED_BACK && targetPhase !== CoordinatorMovePhase.FAILED) return;
    options.setCoordinatorEndpoint(journal.source_url);
    options.link.relocate(journal.source_url);
    await options.relocation.discard();
    return;
  }
  if (journal.state !== "STAGED") return;
  let phase: CoordinatorMovePhase | "gone";
  try {
    phase = (await options.statusAt(journal.source_url, journal.handoff_id)).phase;
  } catch {
    phase = "gone";
  }
  if (phase === "gone" || phase === CoordinatorMovePhase.COMMITTED
    || phase === CoordinatorMovePhase.ROLLED_BACK || phase === CoordinatorMovePhase.FAILED) {
    await options.relocation.discard();
  }
}

async function resolveUnreachableSource(
  options: CoordRelocationRecoveryOptions,
  journal: CoordRelocationJournal,
): Promise<boolean> {
  const [source, target] = await Promise.allSettled([
    options.statusAt(journal.source_url, journal.handoff_id),
    options.statusAt(journal.target_url, journal.handoff_id),
  ]);

  // A reachable source remains authoritative until it reports its own rollback.
  if (source.status === "fulfilled") {
    if (source.value.phase === CoordinatorMovePhase.ROLLED_BACK || source.value.phase === CoordinatorMovePhase.FAILED) {
      // A throwing abortTarget must not strand the local repoint: without this
      // the worker never returns to the rolled-back source.
      await options.abortTarget(journal.handoff_id).catch(() => {});
      await options.relocation.abort(journal.handoff_id, (url) => {
        options.setCoordinatorEndpoint(url);
        options.link.relocate(url);
      });
    }
    return false;
  }
  if (target.status !== "fulfilled") return true;
  // An ACTIVATED journal skips the durable rewrite deliberately, so a target
  // that rolled back after we cut over leaves nothing else to unstick us: the
  // source is unreachable and this phase is not target-owned, so the tick would
  // otherwise re-poll forever. Going back to the source is the correct resting
  // state even while it is down — the reconnect backoff takes it from there.
  if (target.value.phase === CoordinatorMovePhase.ROLLED_BACK || target.value.phase === CoordinatorMovePhase.FAILED) {
    if (journal.state !== "ACTIVATED") return true;
    options.setCoordinatorEndpoint(journal.source_url);
    options.link.relocate(journal.source_url);
    await options.relocation.discard();
    return false;
  }
  if (!TARGET_OWNED_PHASES.has(target.value.phase)) return true;

  // The source is gone and the target authenticated the same handoff. Activate
  // only the runtime journal; the target's COMMIT is still responsible for the
  // durable service-definition cutover after it has received every worker.
  if (journal.state !== "ACTIVATED") {
    await options.relocation.activate({
      handoff_id: journal.handoff_id,
      source_url: journal.source_url,
      target_url: journal.target_url,
    });
  }
  options.setCoordinatorEndpoint(journal.target_url);
  options.link.relocate(journal.target_url);
  options.reannounce(journal.target_url);
  // Settled for this tick: without clearing the gate the 5s driver re-enters
  // immediately and rebuilds the Connect transport on every tick.
  return false;
}
