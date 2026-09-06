// Crash-safe reservation owner for worker-authored durable session events.
// SQLite schema, I/O, and sequence allocation live in focused siblings; this
// facade admits bounded mutations and validates each event before persistence.

import { join } from "node:path";
import {
  AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES,
} from "@roost/shared/agent-conversation-reference";
import { workerDataDir } from "@roost/shared/paths";
import {
  SessionEvent,
  type SessionEvent as SessionEventValue,
} from "@roost/shared/wire";
import { SessionEventStoreDatabase } from "./session-event-store-database.ts";
import {
  SessionEventOutboxFullError,
  SessionEventStoreFatalError,
  sessionEventStoreFatal,
} from "./session-event-store-errors.ts";
import {
  SESSION_EVENT_STORE_MAX_PAYLOAD_BYTES,
  SESSION_EVENT_STORE_MAX_ROWS,
} from "./session-event-store-limits.ts";
import {
  durableSessionEventKind,
  serializedSessionEventLimit,
  type DecodedStoredSessionEvent,
  type DurableSessionEventKind,
} from "./session-event-store-schema.ts";

export {
  SessionEventOutboxFullError,
  SessionEventStoreFatalError,
};
export {
  SESSION_EVENT_SEQUENCE_BLOCK_SIZE,
  SESSION_EVENT_STORE_MAX_DATABASE_BYTES,
  SESSION_EVENT_STORE_MAX_PAYLOAD_BYTES,
  SESSION_EVENT_STORE_MAX_ROWS,
} from "./session-event-store-limits.ts";
export type { DurableSessionEventKind };

const DEFAULT_RESERVED_BYTES: Record<DurableSessionEventKind, number> = {
  opened: 256 * 1024,
  closed: 1_024,
  respawned: 2_048,
  agent_reference: AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES,
};
const reservationBrand: unique symbol = Symbol("SessionEventReservation");
const encoder = new TextEncoder();

export interface SessionEventReservation {
  readonly kind: DurableSessionEventKind;
  readonly payloadBytes: number;
  readonly [reservationBrand]: true;
}

interface InternalReservation extends SessionEventReservation {
  readonly id: number;
  snapshotBlocking: boolean;
}

export type StoredSessionEvent = DecodedStoredSessionEvent;

export interface SessionEventStoreStats {
  pendingRows: number;
  pendingBytes: number;
  reservedRows: number;
  blockingReservedRows: number;
  reservedBytes: number;
}

export interface SessionEventStoreOptions {
  dbPath?: string;
  legacySequencePath?: string;
}

export class SessionEventStore {
  readonly dbPath: string;
  readonly legacySequencePath: string;
  private readonly database: SessionEventStoreDatabase;
  private readonly reservations = new Map<number, InternalReservation>();
  private nextReservationId = 1;
  private reservedRows = 0;
  private blockingReservedRows = 0;
  private reservedBytes = 0;
  private closed = false;

  constructor(options: SessionEventStoreOptions = {}) {
    this.dbPath = options.dbPath ??
      join(workerDataDir(), "session-event-outbox.sqlite");
    this.legacySequencePath = options.legacySequencePath ??
      join(workerDataDir(), "client-seq.txt");
    this.database = new SessionEventStoreDatabase(
      this.dbPath,
      this.legacySequencePath,
    );
  }

  reserveSessionEvent(
    kind: DurableSessionEventKind,
    payloadBytes = DEFAULT_RESERVED_BYTES[kind],
  ): SessionEventReservation {
    this.assertOpen();
    if (!durableSessionEventKind(kind)) {
      throw sessionEventStoreFatal(
        "session event store reservation kind is invalid",
      );
    }
    if (
      !Number.isSafeInteger(payloadBytes) || payloadBytes <= 0 ||
      payloadBytes > SESSION_EVENT_STORE_MAX_PAYLOAD_BYTES ||
      payloadBytes > serializedSessionEventLimit(kind)
    ) {
      throw sessionEventStoreFatal(
        "session event store reservation size is invalid",
      );
    }
    const pending = this.database.stats();
    if (
      pending.pendingRows + this.reservedRows >=
        SESSION_EVENT_STORE_MAX_ROWS ||
      pending.pendingBytes + this.reservedBytes + payloadBytes >
        SESSION_EVENT_STORE_MAX_PAYLOAD_BYTES
    ) {
      throw new SessionEventOutboxFullError();
    }
    const reservation = {
      id: this.nextReservationId++,
      kind,
      payloadBytes,
      snapshotBlocking: true,
      [reservationBrand]: true as const,
    } satisfies InternalReservation;
    this.reservations.set(reservation.id, reservation);
    this.reservedRows++;
    this.reservedBytes += payloadBytes;
    this.blockingReservedRows++;
    return reservation;
  }

  /** A committed live session keeps its future-close capacity without blocking
   * reconnect snapshots. The same token remains the sole owner of that close. */
  holdSessionEvent(reservation: SessionEventReservation): void {
    this.assertOpen();
    const active = this.activeReservation(reservation);
    if (!active.snapshotBlocking) {
      throw sessionEventStoreFatal(
        "session event store reservation is already held",
      );
    }
    active.snapshotBlocking = false;
    this.blockingReservedRows--;
  }

  releaseSessionEvent(reservation: SessionEventReservation): void {
    this.assertOpen();
    const active = this.activeReservation(reservation);
    this.consumeReservation(active);
  }

  appendSessionEvent(
    reservation: SessionEventReservation,
    event: SessionEventValue,
  ): StoredSessionEvent {
    this.assertOpen();
    const active = this.activeReservation(reservation);
    if (event.kind !== active.kind) {
      throw sessionEventStoreFatal(
        "session event store reservation kind mismatch",
      );
    }
    const parsed = SessionEvent.safeParse(event);
    if (
      !parsed.success || parsed.data.kind !== active.kind ||
      !durableSessionEventKind(parsed.data.kind)
    ) {
      throw sessionEventStoreFatal(
        "session event store durable event is invalid",
      );
    }
    let eventJson: string;
    try {
      eventJson = JSON.stringify(parsed.data);
    } catch (error) {
      throw sessionEventStoreFatal(
        "session event store event serialization failed",
        error,
      );
    }
    const payloadBytes = encoder.encode(eventJson).byteLength;
    if (
      payloadBytes <= 0 || payloadBytes > active.payloadBytes ||
      payloadBytes > serializedSessionEventLimit(active.kind)
    ) {
      throw sessionEventStoreFatal(
        "session event store event exceeds reservation",
      );
    }
    const clientSeq = this.database.append(
      active.kind,
      eventJson,
      payloadBytes,
    );
    this.consumeReservation(active);
    return {
      clientSeq,
      kind: active.kind,
      event: parsed.data,
      payloadBytes,
    };
  }

  nextClientSeq(): number {
    this.assertOpen();
    return this.database.nextClientSeq();
  }

  pendingEvents(): StoredSessionEvent[] {
    this.assertOpen();
    return this.database.pendingEvents();
  }

  acknowledge(clientSeq: number): boolean {
    this.assertOpen();
    return this.database.acknowledge(clientSeq);
  }

  stats(): SessionEventStoreStats {
    this.assertOpen();
    const pending = this.database.stats();
    return {
      pendingRows: pending.pendingRows,
      pendingBytes: pending.pendingBytes,
      reservedRows: this.reservedRows,
      reservedBytes: this.reservedBytes,
      blockingReservedRows: this.blockingReservedRows,
    };
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.reservations.clear();
    this.database.close();
  }

  private assertOpen(): void {
    if (this.closed) {
      throw sessionEventStoreFatal("session event store is closed");
    }
  }

  private activeReservation(
    reservation: SessionEventReservation,
  ): InternalReservation {
    const candidate = reservation as InternalReservation;
    const active = typeof candidate?.id === "number"
      ? this.reservations.get(candidate.id)
      : undefined;
    if (!active || active !== candidate) {
      throw sessionEventStoreFatal(
        "session event store reservation is not active",
      );
    }
    return active;
  }

  private consumeReservation(reservation: InternalReservation): void {
    this.reservations.delete(reservation.id);
    this.reservedRows--;
    this.reservedBytes -= reservation.payloadBytes;
    if (reservation.snapshotBlocking) this.blockingReservedRows--;
  }
}

export function openSessionEventStore(
  options: SessionEventStoreOptions = {},
): SessionEventStore {
  return new SessionEventStore(options);
}
