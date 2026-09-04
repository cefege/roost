// Classifies worker session events at their durability boundary.
// Lifecycle events enter the crash-safe store; replaceable metadata coalesces
// in memory. Snapshot publication belongs exclusively to the coord-link barrier.
import type { SessionEvent } from "@roost/shared/wire";
import type { CoordLink } from "./transport/coord-link.ts";
import {
  SessionEventStoreFatalError,
  SessionLifecycleOutboxFullError,
  type DurableLifecycleKind,
  type LifecycleReservation,
  type SessionEventStore,
} from "./transport/session-event-store.ts";

export {
  SessionEventStoreFatalError,
  SessionLifecycleOutboxFullError,
  type DurableLifecycleKind,
  type LifecycleReservation,
} from "./transport/session-event-store.ts";

export type SessionEventClass =
  | { readonly kind: "lifecycle"; readonly lifecycleKind: DurableLifecycleKind }
  | { readonly kind: "metadata"; readonly key: string }
  | { readonly kind: "programmer-error" };

export class SessionEventSinkProgrammerError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "SessionEventSinkProgrammerError";
  }
}

export function isFatalSessionEventError(error: unknown): boolean {
  return error instanceof SessionEventStoreFatalError ||
    error instanceof SessionEventSinkProgrammerError;
}

/** Exhaustive worker-authored event policy. Coordinator-only semantic events
 * deliberately fail at this single boundary rather than entering any queue. */
export function classifySessionEvent(event: SessionEvent): SessionEventClass {
  switch (event.kind) {
    case "opened":
    case "closed":
    case "respawned":
      return { kind: "lifecycle", lifecycleKind: event.kind };
    case "snapshot":
      return { kind: "programmer-error" };
    case "cwd":
    case "git":
    case "pr":
    case "ports":
      return { kind: "metadata", key: `${event.session_id}\u0000${event.kind}` };
    case "attached":
    case "detached":
    case "workspace_assigned":
    case "renamed":
      return { kind: "programmer-error" };
  }
}

export interface SessionEventSink {
  reserveLifecycleEvent(kind: DurableLifecycleKind): LifecycleReservation;
  holdLifecycleEvent(reservation: LifecycleReservation): void;
  releaseLifecycleEvent(reservation: LifecycleReservation): void;
  emit(event: SessionEvent, reservation?: LifecycleReservation): void;
}

/** One synchronous event boundary: the store assigns every sequence, and a
 * lifecycle row is committed before CoordLink can observe it. */
export function coordLinkSink(link: CoordLink, store: SessionEventStore): SessionEventSink {
  return {
    reserveLifecycleEvent(kind) {
      return store.reserveLifecycleEvent(kind);
    },
    holdLifecycleEvent(reservation) {
      store.holdLifecycleEvent(reservation);
      link.snapshotStateChanged();
    },
    releaseLifecycleEvent(reservation) {
      store.releaseLifecycleEvent(reservation);
      link.snapshotStateChanged();
    },
    emit(event, reservation) {
      const classification = classifySessionEvent(event);
      if (classification.kind === "programmer-error") {
        const reason = event.kind === "snapshot"
          ? "snapshot events are owned by the coord-link barrier"
          : `worker must not emit coordinator-authored session event: ${event.kind}`;
        throw new SessionEventSinkProgrammerError(reason);
      }
      if (classification.kind === "lifecycle") {
        if (!reservation) {
          throw new SessionEventSinkProgrammerError(
            `durable lifecycle event requires a reservation: ${event.kind}`,
          );
        }
        let stored;
        try {
          stored = store.appendLifecycleEvent(reservation, event);
        } catch (error) {
          // Capacity was guaranteed at reservation time. Any later failure is
          // a fatal durability failure, including an unexpected SQLITE_FULL.
          if (error instanceof SessionLifecycleOutboxFullError) {
            throw new SessionEventStoreFatalError(
              "session event store append exhausted reserved capacity",
              { cause: error },
            );
          }
          throw error;
        }
        link.send({
          kind: "event",
          event: stored.event,
          clientSeq: stored.clientSeq,
          eventClass: "lifecycle",
        });
        return;
      }
      const clientSeq = store.nextClientSeq();
      link.send({
        kind: "event",
        event,
        clientSeq,
        eventClass: "metadata",
        metadataKey: classification.key,
      });
    },
  };
}
