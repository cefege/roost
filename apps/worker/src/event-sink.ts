// Classifies worker session events at their durability boundary.
// Durable session events enter the crash-safe store; replaceable metadata
// coalesces in memory. Snapshot publication belongs to the CoordLink barrier.
import type { SessionEvent } from "@roost/shared/wire";
import type { CoordLink } from "./transport/coord-link.ts";
import {
  SessionEventStoreFatalError,
  SessionEventOutboxFullError,
  type DurableSessionEventKind,
  type SessionEventReservation,
  type SessionEventStore,
} from "./transport/session-event-store.ts";

export {
  SessionEventStoreFatalError,
  SessionEventOutboxFullError,
  type DurableSessionEventKind,
  type SessionEventReservation,
} from "./transport/session-event-store.ts";

export type SessionEventClass =
  | { readonly kind: "durable"; readonly durableKind: DurableSessionEventKind }
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
    case "agent_reference":
      return { kind: "durable", durableKind: event.kind };
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
  reserveSessionEvent(kind: DurableSessionEventKind): SessionEventReservation;
  holdSessionEvent(reservation: SessionEventReservation): void;
  releaseSessionEvent(reservation: SessionEventReservation): void;
  emit(event: SessionEvent, reservation?: SessionEventReservation): void;
}

/** One synchronous event boundary: the store assigns every sequence, and a
 * durable row is committed before CoordLink can observe it. */
export function coordLinkSink(link: CoordLink, store: SessionEventStore): SessionEventSink {
  return {
    reserveSessionEvent(kind) {
      const reservation = store.reserveSessionEvent(kind);
      link.snapshotStateChanged();
      return reservation;
    },
    holdSessionEvent(reservation) {
      store.holdSessionEvent(reservation);
      link.snapshotStateChanged();
    },
    releaseSessionEvent(reservation) {
      store.releaseSessionEvent(reservation);
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
      if (classification.kind === "durable") {
        if (!reservation) {
          throw new SessionEventSinkProgrammerError(
            `durable session event requires a reservation: ${event.kind}`,
          );
        }
        let stored;
        try {
          stored = store.appendSessionEvent(reservation, event);
        } catch (error) {
          // Capacity was guaranteed at reservation time. Any later failure is
          // a fatal durability failure, including an unexpected SQLITE_FULL.
          if (error instanceof SessionEventOutboxFullError) {
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
          eventClass: "durable",
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
