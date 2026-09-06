// Owns crash-safe client-sequence block allocation for SessionEventStore.
// It imports the retired text watermark once and persists every new block
// before handing any value to durable, snapshot, or metadata publication.

import type { Database } from "bun:sqlite";
import { existsSync, readFileSync, unlinkSync } from "node:fs";
import { sessionEventStoreFatal } from "./session-event-store-errors.ts";
import { SESSION_EVENT_SEQUENCE_BLOCK_SIZE } from "./session-event-store-limits.ts";
import { MAX_SESSION_EVENT_SEQUENCE } from "./session-event-store-schema.ts";

interface SequenceState {
  reserved_through: number;
  legacy_imported: number;
}

type TransactionRunner = (label: string, body: () => void) => void;

export class SessionEventSequenceAllocator {
  private currentSequence = 0;
  private reservedThrough = 0;

  constructor(
    private readonly db: Database,
    private readonly legacySequencePath: string,
    private readonly transaction: TransactionRunner,
    maximumPersistedSequence: number,
  ) {
    const state = this.readState();
    if (maximumPersistedSequence > state.reserved_through) {
      throw sessionEventStoreFatal(
        "session event store sequence state is unsafe",
      );
    }
    const legacy = state.legacy_imported === 0
      ? readLegacySequence(legacySequencePath)
      : null;
    const floor = Math.max(state.reserved_through, legacy ?? 0);
    const end = blockEnd(floor);
    transaction("sequence reservation", () => {
      const current = this.readState();
      if (
        current.reserved_through !== state.reserved_through ||
        current.legacy_imported !== state.legacy_imported
      ) {
        throw sessionEventStoreFatal(
          "session event store sequence state changed concurrently",
        );
      }
      db.query(
        "UPDATE sequence_state SET reserved_through=?,legacy_imported=1 WHERE singleton=1",
      ).run(end);
    });
    this.currentSequence = floor;
    this.reservedThrough = end;
    if (state.legacy_imported === 0 && legacy !== null) {
      try {
        unlinkSync(legacySequencePath);
      } catch {
        // The imported watermark cannot affect this initialized database.
      }
    }
  }

  next(): number {
    if (this.currentSequence >= this.reservedThrough) this.reserveNextBlock();
    const next = this.currentSequence + 1;
    if (
      !Number.isSafeInteger(next) || next > this.reservedThrough ||
      next > MAX_SESSION_EVENT_SEQUENCE
    ) {
      throw sessionEventStoreFatal("session event store sequence exhausted");
    }
    this.currentSequence = next;
    return next;
  }

  private reserveNextBlock(): void {
    const before = this.readState();
    if (before.legacy_imported !== 1) {
      throw sessionEventStoreFatal(
        "session event store sequence state is unsafe",
      );
    }
    const floor = Math.max(this.currentSequence, before.reserved_through);
    const end = blockEnd(floor);
    this.transaction("sequence reservation", () => {
      const current = this.readState();
      if (
        current.reserved_through !== before.reserved_through ||
        current.legacy_imported !== 1
      ) {
        throw sessionEventStoreFatal(
          "session event store sequence state changed concurrently",
        );
      }
      this.db.query(
        "UPDATE sequence_state SET reserved_through=? WHERE singleton=1",
      ).run(end);
    });
    this.currentSequence = floor;
    this.reservedThrough = end;
  }

  private readState(): SequenceState {
    const row = this.db.query(
      "SELECT reserved_through,legacy_imported FROM sequence_state WHERE singleton=1",
    ).get() as SequenceState | null;
    if (
      !row || !safeSequence(row.reserved_through) ||
      (row.legacy_imported !== 0 && row.legacy_imported !== 1)
    ) {
      throw sessionEventStoreFatal(
        "session event store sequence state is unsafe",
      );
    }
    return row;
  }
}

function safeSequence(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function blockEnd(floor: number): number {
  if (
    !safeSequence(floor) ||
    floor > MAX_SESSION_EVENT_SEQUENCE - SESSION_EVENT_SEQUENCE_BLOCK_SIZE
  ) {
    throw sessionEventStoreFatal("session event store sequence exhausted");
  }
  return floor + SESSION_EVENT_SEQUENCE_BLOCK_SIZE;
}

function readLegacySequence(path: string): number | null {
  if (!existsSync(path)) return null;
  let value: string;
  try {
    value = readFileSync(path, "utf8").trim();
  } catch (error) {
    throw sessionEventStoreFatal(
      "session event store legacy sequence read failed",
      error,
    );
  }
  if (!/^(?:0|[1-9][0-9]*)$/.test(value)) {
    throw sessionEventStoreFatal(
      "session event store legacy sequence is invalid",
    );
  }
  const parsed = Number(value);
  if (!safeSequence(parsed)) {
    throw sessionEventStoreFatal(
      "session event store legacy sequence is unsafe",
    );
  }
  return parsed;
}
