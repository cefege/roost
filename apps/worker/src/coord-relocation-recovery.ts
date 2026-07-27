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
  now?: () => number;
  abortTarget: (handoffId: string) => Promise<void>;
  unavailableAfterMs?: number;
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

  return async (): Promise<void> => {
    if (running) return;
    const journal = options.relocation.load();
    if (!journal || options.link.state().kind === "open") {
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
      await options.abortTarget(journal.handoff_id);
      await options.relocation.abort((url) => {
        options.setCoordinatorEndpoint(url);
        options.link.relocate(url);
      });
    }
    return false;
  }
  if (target.status !== "fulfilled") return true;
  if (!TARGET_OWNED_PHASES.has(target.value.phase)) return true;

  // The source is gone and the target authenticated the same handoff. Activate
  // only the runtime journal; the target's COMMIT is still responsible for the
  // durable LaunchAgent cutover after it has received every worker.
  options.relocation.activate({
    handoff_id: journal.handoff_id,
    source_url: journal.source_url,
    target_url: journal.target_url,
  });
  options.setCoordinatorEndpoint(journal.target_url);
  options.link.relocate(journal.target_url);
  return true;
}
