// In-memory SessionEventSink used by focused worker tests.
// It preserves the production reservation ownership rules without opening
// SQLite or connecting to the coordinator.
import type { SessionEvent } from "@roost/shared/wire";
import {
  SessionEventStoreFatalError,
  SessionEventOutboxFullError,
  type DurableSessionEventKind,
  type SessionEventReservation,
  type SessionEventSink,
} from "../src/event-sink.ts";

interface TestReservation {
  kind: DurableSessionEventKind;
  payloadBytes: number;
  snapshotBlocking: boolean;
}

export class SessionEventTestSink implements SessionEventSink {
  readonly events: SessionEvent[] = [];
  readonly active = new Set<TestReservation>();
  capacity: number;
  failNextEmit = false;

  constructor(capacity = Number.MAX_SAFE_INTEGER) {
    this.capacity = capacity;
  }

  reserveSessionEvent(kind: DurableSessionEventKind): SessionEventReservation {
    if (this.active.size >= this.capacity) {
      throw new SessionEventOutboxFullError();
    }
    const reservation: TestReservation = {
      kind,
      payloadBytes: 256 * 1024,
      snapshotBlocking: true,
    };
    this.active.add(reservation);
    return reservation as unknown as SessionEventReservation;
  }

  holdSessionEvent(reservation: SessionEventReservation): void {
    const owned = reservation as unknown as TestReservation;
    if (!this.active.has(owned) || !owned.snapshotBlocking) {
      throw new SessionEventStoreFatalError("test reservation cannot be held");
    }
    owned.snapshotBlocking = false;
  }

  releaseSessionEvent(reservation: SessionEventReservation): void {
    const owned = reservation as unknown as TestReservation;
    if (!this.active.delete(owned)) {
      throw new SessionEventStoreFatalError("test reservation was already consumed");
    }
  }

  emit(event: SessionEvent, reservation?: SessionEventReservation): void {
    if (this.failNextEmit) {
      this.failNextEmit = false;
      throw new SessionEventStoreFatalError("injected session event append failure");
    }
    if (
      event.kind === "opened" || event.kind === "closed" ||
      event.kind === "respawned" || event.kind === "agent_reference"
    ) {
      if (!reservation) {
        throw new SessionEventStoreFatalError("test durable event lacked reservation");
      }
      const owned = reservation as unknown as TestReservation;
      if (owned.kind !== event.kind || !this.active.delete(owned)) {
        throw new SessionEventStoreFatalError(
          "test session event reservation mismatch",
        );
      }
    }
    this.events.push(event);
  }
}
