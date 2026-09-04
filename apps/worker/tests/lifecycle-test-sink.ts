import type { SessionEvent } from "@roost/shared/wire";
import {
  SessionEventStoreFatalError,
  SessionLifecycleOutboxFullError,
  type DurableLifecycleKind,
  type LifecycleReservation,
  type SessionEventSink,
} from "../src/event-sink.ts";

interface TestReservation {
  kind: DurableLifecycleKind;
  payloadBytes: number;
  snapshotBlocking: boolean;
}

export class LifecycleTestSink implements SessionEventSink {
  readonly events: SessionEvent[] = [];
  readonly active = new Set<TestReservation>();
  capacity: number;
  failNextEmit = false;

  constructor(capacity = Number.MAX_SAFE_INTEGER) {
    this.capacity = capacity;
  }

  reserveLifecycleEvent(kind: DurableLifecycleKind): LifecycleReservation {
    if (this.active.size >= this.capacity) {
      throw new SessionLifecycleOutboxFullError();
    }
    const reservation: TestReservation = {
      kind,
      payloadBytes: 256 * 1024,
      snapshotBlocking: true,
    };
    this.active.add(reservation);
    return reservation as unknown as LifecycleReservation;
  }

  holdLifecycleEvent(reservation: LifecycleReservation): void {
    const owned = reservation as unknown as TestReservation;
    if (!this.active.has(owned) || !owned.snapshotBlocking) {
      throw new SessionEventStoreFatalError("test reservation cannot be held");
    }
    owned.snapshotBlocking = false;
  }

  releaseLifecycleEvent(reservation: LifecycleReservation): void {
    const owned = reservation as unknown as TestReservation;
    if (!this.active.delete(owned)) {
      throw new SessionEventStoreFatalError("test reservation was already consumed");
    }
  }

  emit(event: SessionEvent, reservation?: LifecycleReservation): void {
    if (this.failNextEmit) {
      this.failNextEmit = false;
      throw new SessionEventStoreFatalError("injected lifecycle append failure");
    }
    if (event.kind === "opened" || event.kind === "closed" || event.kind === "respawned") {
      if (!reservation) {
        throw new SessionEventStoreFatalError("test lifecycle event lacked reservation");
      }
      const owned = reservation as unknown as TestReservation;
      if (owned.kind !== event.kind || !this.active.delete(owned)) {
        throw new SessionEventStoreFatalError("test lifecycle reservation mismatch");
      }
    }
    this.events.push(event);
  }
}
