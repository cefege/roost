// Owns SQLite I/O and crash-safe sequence allocation for SessionEventStore.
// The reservation owner stays in session-event-store.ts; this layer opens,
// migrates, validates, proves retained appends, ACKs, and closes the v2 database.

import { Database } from "bun:sqlite";
import { mkdirSync } from "node:fs";
import { dirname } from "node:path";
import {
  SessionEventOutboxFullError,
  SessionEventStoreFatalError,
  sessionEventStoreFatal,
} from "./session-event-store-errors.ts";
import {
  SESSION_EVENT_STORE_MAX_DATABASE_BYTES,
  SESSION_EVENT_STORE_MAX_PAYLOAD_BYTES,
  SESSION_EVENT_STORE_MAX_ROWS,
} from "./session-event-store-limits.ts";
import {
  decodeStoredSessionEventRow,
  ensureSessionEventStoreSchema,
  type DurableSessionEventKind,
  type DecodedStoredSessionEvent,
  type StoredSessionEventRow,
} from "./session-event-store-schema.ts";
import { SessionEventSequenceAllocator } from "./session-event-store-sequence.ts";


export class SessionEventStoreDatabase {
  private readonly db: Database;
  private sequence!: SessionEventSequenceAllocator;
  private pendingRows = 0;
  private pendingBytes = 0;
  private closed = false;

  constructor(
    readonly dbPath: string,
    readonly legacySequencePath: string,
  ) {
    try {
      mkdirSync(dirname(dbPath), { recursive: true });
    } catch (error) {
      throw sessionEventStoreFatal(
        "session event store directory creation failed",
        error,
      );
    }
    try {
      this.db = new Database(dbPath, { create: true });
    } catch (error) {
      throw sessionEventStoreFatal("session event store open failed", error);
    }
    try {
      this.configure();
      this.checkIntegrity();
      ensureSessionEventStoreSchema(
        this.db,
        (label, body) => this.transaction(label, body),
        {
          maxRows: SESSION_EVENT_STORE_MAX_ROWS,
          maxPayloadBytes: SESSION_EVENT_STORE_MAX_PAYLOAD_BYTES,
        },
      );
      this.enforcePageLimit();
      const maximumPersistedSequence = this.loadRows();
      this.sequence = new SessionEventSequenceAllocator(
        this.db,
        legacySequencePath,
        (label, body) => this.transaction(label, body),
        maximumPersistedSequence,
      );
    } catch (error) {
      try {
        this.db.close();
      } catch {
        // Preserve the initialization failure.
      }
      if (error instanceof SessionEventStoreFatalError) throw error;
      throw sessionEventStoreFatal(
        "session event store initialization failed",
        error,
      );
    }
  }

  stats(): { pendingRows: number; pendingBytes: number } {
    this.assertOpen();
    return { pendingRows: this.pendingRows, pendingBytes: this.pendingBytes };
  }

  nextClientSeq(): number {
    this.assertOpen();
    return this.sequence.next();
  }

  append(
    kind: DurableSessionEventKind,
    eventJson: string,
    payloadBytes: number,
  ): number {
    const clientSeq = this.nextClientSeq();
    this.transaction("append", () => {
      this.db.query(
        "INSERT INTO session_events (client_seq, kind, event_json, payload_bytes) VALUES (?, ?, ?, ?)",
      ).run(clientSeq, kind, eventJson, payloadBytes);
      const retained = this.db.query(`
        SELECT
          COUNT(*) AS retained_rows,
          COALESCE(SUM(
            CASE
              WHEN kind = ? AND event_json = ? AND payload_bytes = ? THEN 1
              ELSE 0
            END
          ), 0) AS matching_rows
        FROM session_events
        WHERE client_seq = ?
      `).get(kind, eventJson, payloadBytes, clientSeq) as {
        retained_rows: number;
        matching_rows: number;
      } | null;
      if (
        !retained || retained.retained_rows !== 1 ||
        retained.matching_rows !== 1
      ) {
        throw sessionEventStoreFatal(
          "session event store append verification failed",
        );
      }
    });
    this.pendingRows++;
    this.pendingBytes += payloadBytes;
    return clientSeq;
  }

  pendingEvents(): DecodedStoredSessionEvent[] {
    this.assertOpen();
    try {
      const rows = this.db.query(
        "SELECT client_seq,kind,event_json,payload_bytes FROM session_events ORDER BY client_seq",
      ).all() as StoredSessionEventRow[];
      return rows.map(decodeStoredSessionEventRow);
    } catch (error) {
      if (error instanceof SessionEventStoreFatalError) throw error;
      throw sessionEventStoreFatal("session event store read failed", error);
    }
  }

  acknowledge(clientSeq: number): boolean {
    this.assertOpen();
    if (!Number.isSafeInteger(clientSeq) || clientSeq <= 0) return false;
    let payloadBytes: number | null = null;
    this.transaction("ACK", () => {
      const row = this.db.query(
        "SELECT payload_bytes FROM session_events WHERE client_seq = ?",
      ).get(clientSeq) as { payload_bytes: number } | null;
      if (!row) return;
      if (
        !Number.isSafeInteger(row.payload_bytes) || row.payload_bytes <= 0 ||
        this.pendingRows <= 0 || row.payload_bytes > this.pendingBytes
      ) {
        throw sessionEventStoreFatal(
          "session event store ACK row is corrupt",
        );
      }
      payloadBytes = row.payload_bytes;
      const deleted = this.db.query(
        "DELETE FROM session_events WHERE client_seq = ?",
      ).run(clientSeq);
      if (deleted.changes !== 1) {
        throw sessionEventStoreFatal(
          "session event store ACK delete mismatch",
        );
      }
    });
    if (payloadBytes === null) return false;
    this.pendingRows--;
    this.pendingBytes -= payloadBytes;
    return true;
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    try {
      this.db.close();
    } catch (error) {
      throw sessionEventStoreFatal("session event store close failed", error);
    }
  }

  private assertOpen(): void {
    if (this.closed) {
      throw sessionEventStoreFatal("session event store is closed");
    }
  }

  private transaction(label: string, body: () => void): void {
    let open = false;
    try {
      this.db.exec("BEGIN IMMEDIATE");
      open = true;
      body();
      this.db.exec("COMMIT");
      open = false;
    } catch (error) {
      if (open) {
        try {
          this.db.exec("ROLLBACK");
        } catch {
          // Preserve the original transaction failure.
        }
      }
      if (error instanceof SessionEventStoreFatalError) throw error;
      if (label === "append" && sqliteFull(error)) {
        throw new SessionEventOutboxFullError();
      }
      throw sessionEventStoreFatal(`session event store ${label} failed`, error);
    }
  }

  private configure(): void {
    const journal = this.db.query("PRAGMA journal_mode = DELETE").get() as {
      journal_mode?: string;
    } | null;
    if (journal?.journal_mode?.toLowerCase() !== "delete") {
      throw sessionEventStoreFatal(
        "session event store could not enable DELETE journal mode",
      );
    }
    this.db.exec("PRAGMA synchronous = FULL");
    const sync = this.db.query("PRAGMA synchronous").get() as {
      synchronous?: number;
    } | null;
    if (sync?.synchronous !== 2) {
      throw sessionEventStoreFatal(
        "session event store could not enable FULL synchronous mode",
      );
    }
  }

  private checkIntegrity(): void {
    let rows: Array<Record<string, unknown>>;
    try {
      rows = this.db.query("PRAGMA integrity_check").all() as Array<
        Record<string, unknown>
      >;
    } catch (error) {
      throw sessionEventStoreFatal(
        "session event store integrity check failed",
        error,
      );
    }
    if (rows.length !== 1 || Object.values(rows[0] ?? {})[0] !== "ok") {
      throw sessionEventStoreFatal(
        "session event store integrity check failed",
      );
    }
  }

  private enforcePageLimit(): void {
    const page = this.db.query("PRAGMA page_size").get() as {
      page_size?: number;
    } | null;
    if (!page?.page_size || !Number.isSafeInteger(page.page_size)) {
      throw sessionEventStoreFatal(
        "session event store page size is invalid",
      );
    }
    const maxPages = Math.floor(
      SESSION_EVENT_STORE_MAX_DATABASE_BYTES / page.page_size,
    );
    const set = this.db.query(`PRAGMA max_page_count=${maxPages}`).get() as {
      max_page_count?: number;
    } | null;
    const count = this.db.query("PRAGMA page_count").get() as {
      page_count?: number;
    } | null;
    if (
      set?.max_page_count !== maxPages ||
      !Number.isSafeInteger(count?.page_count) || count!.page_count! > maxPages
    ) {
      throw sessionEventStoreFatal(
        "session event store database exceeds size limit",
      );
    }
  }

  private loadRows(): number {
    let bytes = 0;
    let maximumPersistedSequence = 0;
    const rows = this.db.query(
      "SELECT client_seq,kind,event_json,payload_bytes FROM session_events ORDER BY client_seq",
    ).all() as StoredSessionEventRow[];
    if (rows.length > SESSION_EVENT_STORE_MAX_ROWS) {
      throw sessionEventStoreFatal("session event store row limit exceeded");
    }
    for (const row of rows) {
      decodeStoredSessionEventRow(row);
      bytes += row.payload_bytes;
      maximumPersistedSequence = Math.max(
        maximumPersistedSequence,
        row.client_seq,
      );
    }
    if (
      !Number.isSafeInteger(bytes) ||
      bytes > SESSION_EVENT_STORE_MAX_PAYLOAD_BYTES
    ) {
      throw sessionEventStoreFatal(
        "session event store payload limit exceeded",
      );
    }
    this.pendingRows = rows.length;
    this.pendingBytes = bytes;
    return maximumPersistedSequence;
  }
}


function sqliteFull(error: unknown): boolean {
  if (!error || typeof error !== "object" || !("code" in error)) return false;
  const code = error.code;
  return code === "SQLITE_FULL" || code === 13;
}
