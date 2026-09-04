// Crash-safe source of truth for worker-authored session lifecycle events.
// Bounded reservations admit mutations before PTY state changes, while a
// durable sequence watermark orders lifecycle, snapshots, and metadata.
import { Database } from "bun:sqlite";
import { existsSync, mkdirSync, readFileSync, unlinkSync } from "node:fs";
import { dirname, join } from "node:path";
import { workerDataDir } from "@roost/shared/paths";
import { SessionEvent, type SessionEvent as SessionEventValue } from "@roost/shared/wire";

export const SESSION_EVENT_STORE_MAX_ROWS = 8_192;
export const SESSION_EVENT_STORE_MAX_PAYLOAD_BYTES = 8 * 1024 * 1024;
export const SESSION_EVENT_STORE_MAX_DATABASE_BYTES = 16 * 1024 * 1024;
export const SESSION_EVENT_SEQUENCE_BLOCK_SIZE = 1_024;
const SCHEMA_VERSION = 1;
const MAX_SEQ = Number.MAX_SAFE_INTEGER;
const encoder = new TextEncoder();

export type DurableLifecycleKind = "opened" | "closed" | "respawned";
const DEFAULT_RESERVED_BYTES: Record<DurableLifecycleKind, number> = {
  opened: 256 * 1024,
  closed: 1_024,
  respawned: 2_048,
};
const reservationBrand: unique symbol = Symbol("LifecycleReservation");
export interface LifecycleReservation {
  readonly kind: DurableLifecycleKind;
  readonly payloadBytes: number;
  readonly [reservationBrand]: true;
}
interface InternalReservation extends LifecycleReservation {
  readonly id: number;
  snapshotBlocking: boolean;
}
export interface StoredLifecycleEvent {
  readonly clientSeq: number;
  readonly kind: DurableLifecycleKind;
  readonly event: SessionEventValue;
  readonly payloadBytes: number;
}
export interface SessionEventStoreStats {
  pendingRows: number;
  pendingBytes: number;
  reservedRows: number;
  blockingReservedRows: number;
  reservedBytes: number;
}
export interface SessionEventStoreOptions { dbPath?: string; legacySequencePath?: string }

export class SessionLifecycleOutboxFullError extends Error {
  constructor() { super("session lifecycle outbox full"); this.name = "SessionLifecycleOutboxFullError"; }
}
export class SessionEventStoreFatalError extends Error {
  constructor(message: string, options?: ErrorOptions) { super(message, options); this.name = "SessionEventStoreFatalError"; }
}
function fatal(message: string, cause?: unknown): SessionEventStoreFatalError {
  return new SessionEventStoreFatalError(message, cause === undefined ? undefined : { cause });
}
function durableKind(value: unknown): DurableLifecycleKind | null {
  return value === "opened" || value === "closed" || value === "respawned" ? value : null;
}
function safeSeq(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}
function byteLength(value: string): number { return encoder.encode(value).byteLength; }
function readLegacySequence(path: string): number | null {
  if (!existsSync(path)) return null;
  let value: string;
  try { value = readFileSync(path, "utf8").trim(); }
  catch (error) { throw fatal("session event store legacy sequence read failed", error); }
  if (!/^(?:0|[1-9][0-9]*)$/.test(value)) throw fatal("session event store legacy sequence is invalid");
  const parsed = Number(value);
  if (!safeSeq(parsed)) throw fatal("session event store legacy sequence is unsafe");
  return parsed;
}

export class SessionEventStore {
  readonly dbPath: string;
  readonly legacySequencePath: string;
  private readonly db: Database;
  private readonly reservations = new Map<number, InternalReservation>();
  private nextReservationId = 1;
  private pendingRows = 0;
  private pendingBytes = 0;
  private reservedRows = 0;
  private blockingReservedRows = 0;
  private reservedBytes = 0;
  private currentSequence = 0;
  private reservedThrough = 0;
  private closed = false;

  constructor(options: SessionEventStoreOptions = {}) {
    this.dbPath = options.dbPath ?? join(workerDataDir(), "session-event-outbox.sqlite");
    this.legacySequencePath = options.legacySequencePath ?? join(workerDataDir(), "client-seq.txt");
    try { mkdirSync(dirname(this.dbPath), { recursive: true }); }
    catch (error) { throw fatal("session event store directory creation failed", error); }
    try { this.db = new Database(this.dbPath, { create: true }); }
    catch (error) { throw fatal("session event store open failed", error); }
    try {
      this.configure();
      this.checkIntegrity();
      this.ensureSchema();
      this.enforcePageLimit();
      this.loadRows();
      this.reserveInitialBlock();
    } catch (error) {
      try { this.db.close(); } catch { /* preserve original */ }
      if (error instanceof SessionEventStoreFatalError) throw error;
      throw fatal("session event store initialization failed", error);
    }
  }

  reserveLifecycleEvent(kind: DurableLifecycleKind, payloadBytes = DEFAULT_RESERVED_BYTES[kind]): LifecycleReservation {
    this.assertOpen();
    if (!durableKind(kind)) throw fatal("session event store reservation kind is invalid");
    if (!Number.isSafeInteger(payloadBytes) || payloadBytes <= 0 || payloadBytes > SESSION_EVENT_STORE_MAX_PAYLOAD_BYTES) {
      throw fatal("session event store reservation size is invalid");
    }
    if (this.pendingRows + this.reservedRows >= SESSION_EVENT_STORE_MAX_ROWS ||
        this.pendingBytes + this.reservedBytes + payloadBytes > SESSION_EVENT_STORE_MAX_PAYLOAD_BYTES) {
      throw new SessionLifecycleOutboxFullError();
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
  holdLifecycleEvent(reservation: LifecycleReservation): void {
    this.assertOpen();
    const active = this.activeReservation(reservation);
    if (!active.snapshotBlocking) {
      throw fatal("session event store reservation is already held");
    }
    active.snapshotBlocking = false;
    this.blockingReservedRows--;
  }

  releaseLifecycleEvent(reservation: LifecycleReservation): void {
    this.assertOpen();
    const active = this.activeReservation(reservation);
    this.reservations.delete(active.id);
    this.reservedRows--;
    this.reservedBytes -= active.payloadBytes;
    if (active.snapshotBlocking) this.blockingReservedRows--;
  }

  appendLifecycleEvent(reservation: LifecycleReservation, event: SessionEventValue): StoredLifecycleEvent {
    this.assertOpen();
    const active = this.activeReservation(reservation);
    if (event.kind !== active.kind) throw fatal("session event store reservation kind mismatch");
    const parsed = SessionEvent.safeParse(event);
    if (!parsed.success || !durableKind(parsed.data.kind)) throw fatal("session event store lifecycle event is invalid");
    let eventJson: string;
    try { eventJson = JSON.stringify(parsed.data); }
    catch (error) { throw fatal("session event store lifecycle serialization failed", error); }
    const payloadBytes = byteLength(eventJson);
    if (payloadBytes <= 0 || payloadBytes > active.payloadBytes) {
      throw fatal("session event store lifecycle event exceeds reservation");
    }
    const clientSeq = this.nextClientSeq();
    this.transaction("append", () => {
      this.db.query("INSERT INTO lifecycle_events (client_seq, kind, event_json, payload_bytes) VALUES (?, ?, ?, ?)")
        .run(clientSeq, active.kind, eventJson, payloadBytes);
    });
    this.reservations.delete(active.id);
    this.reservedRows--;
    this.reservedBytes -= active.payloadBytes;
    if (active.snapshotBlocking) this.blockingReservedRows--;
    this.pendingRows++;
    this.pendingBytes += payloadBytes;
    return { clientSeq, kind: active.kind, event: parsed.data, payloadBytes };
  }

  nextClientSeq(): number {
    this.assertOpen();
    if (this.currentSequence >= this.reservedThrough) this.reserveNextBlock();
    const next = this.currentSequence + 1;
    if (!Number.isSafeInteger(next) || next > this.reservedThrough || next > MAX_SEQ) throw fatal("session event store sequence exhausted");
    this.currentSequence = next;
    return next;
  }

  pendingEvents(): StoredLifecycleEvent[] {
    this.assertOpen();
    try {
      const rows = this.db.query("SELECT client_seq, kind, event_json, payload_bytes FROM lifecycle_events ORDER BY client_seq").all() as StoredRow[];
      return rows.map((row) => this.validateRow(row));
    } catch (error) {
      if (error instanceof SessionEventStoreFatalError) throw error;
      throw fatal("session event store read failed", error);
    }
  }

  acknowledge(clientSeq: number): boolean {
    this.assertOpen();
    if (!Number.isSafeInteger(clientSeq) || clientSeq <= 0) return false;
    let payloadBytes: number | null = null;
    this.transaction("ACK", () => {
      const row = this.db.query(
        "SELECT payload_bytes FROM lifecycle_events WHERE client_seq = ?",
      ).get(clientSeq) as { payload_bytes: number } | null;
      if (!row) return;
      if (
        !Number.isSafeInteger(row.payload_bytes)
        || row.payload_bytes <= 0
        || this.pendingRows <= 0
        || row.payload_bytes > this.pendingBytes
      ) {
        throw fatal("session event store ACK row is corrupt");
      }
      payloadBytes = row.payload_bytes;
      const deleted = this.db.query(
        "DELETE FROM lifecycle_events WHERE client_seq = ?",
      ).run(clientSeq);
      if (deleted.changes !== 1) {
        throw fatal("session event store ACK delete mismatch");
      }
    });
    if (payloadBytes === null) return false;
    this.pendingRows--;
    this.pendingBytes -= payloadBytes;
    return true;
  }

  stats(): SessionEventStoreStats {
    this.assertOpen();
    return {
      pendingRows: this.pendingRows,
      pendingBytes: this.pendingBytes,
      reservedRows: this.reservedRows,
      reservedBytes: this.reservedBytes,
      blockingReservedRows: this.blockingReservedRows,
    };
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.reservations.clear();
    try { this.db.close(); }
    catch (error) { throw fatal("session event store close failed", error); }
  }

  private assertOpen(): void { if (this.closed) throw fatal("session event store is closed"); }
  private activeReservation(reservation: LifecycleReservation): InternalReservation {
    const candidate = reservation as InternalReservation;
    const active = typeof candidate?.id === "number" ? this.reservations.get(candidate.id) : undefined;
    if (!active || active !== candidate) throw fatal("session event store reservation is not active");
    return active;
  }
  private transaction(label: string, body: () => void): void {
    let open = false;
    try {
      this.db.exec("BEGIN IMMEDIATE"); open = true;
      body();
      this.db.exec("COMMIT"); open = false;
    } catch (error) {
      if (open) { try { this.db.exec("ROLLBACK"); } catch { /* preserve original */ } }
      if (error instanceof SessionEventStoreFatalError) throw error;
      if (label === "append" && this.sqliteFull(error)) throw new SessionLifecycleOutboxFullError();
      throw fatal(`session event store ${label} failed`, error);
    }
  }
  private configure(): void {
    const journal = this.db.query("PRAGMA journal_mode = DELETE").get() as { journal_mode?: string } | null;
    if (journal?.journal_mode?.toLowerCase() !== "delete") throw fatal("session event store could not enable DELETE journal mode");
    this.db.exec("PRAGMA synchronous = FULL");
    const sync = this.db.query("PRAGMA synchronous").get() as { synchronous?: number } | null;
    if (sync?.synchronous !== 2) throw fatal("session event store could not enable FULL synchronous mode");
  }
  private checkIntegrity(): void {
    let rows: Array<Record<string, unknown>>;
    try { rows = this.db.query("PRAGMA integrity_check").all() as Array<Record<string, unknown>>; }
    catch (error) { throw fatal("session event store integrity check failed", error); }
    if (rows.length !== 1 || Object.values(rows[0] ?? {})[0] !== "ok") throw fatal("session event store integrity check failed");
  }
  private ensureSchema(): void {
    const tables = this.db.query("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").all() as Array<{ name: string }>;
    if (tables.length === 0) {
      this.transaction("schema creation", () => this.db.exec(`
        CREATE TABLE sequence_state (
          singleton INTEGER PRIMARY KEY CHECK(singleton=1),
          reserved_through INTEGER NOT NULL CHECK(reserved_through BETWEEN 0 AND ${MAX_SEQ}),
          legacy_imported INTEGER NOT NULL CHECK(legacy_imported IN (0,1))
        ) STRICT;
        CREATE TABLE lifecycle_events (
          client_seq INTEGER PRIMARY KEY CHECK(client_seq > 0 AND client_seq <= ${MAX_SEQ}),
          kind TEXT NOT NULL CHECK(kind IN ('opened','closed','respawned')),
          event_json TEXT NOT NULL,
          payload_bytes INTEGER NOT NULL CHECK(payload_bytes > 0 AND payload_bytes <= ${SESSION_EVENT_STORE_MAX_PAYLOAD_BYTES})
        ) STRICT;
        INSERT INTO sequence_state VALUES (1,0,0);
        PRAGMA user_version=${SCHEMA_VERSION};
      `));
      return;
    }
    if (tables.map((x) => x.name).join(",") !== "lifecycle_events,sequence_state") throw fatal("session event store schema mismatch");
    const version = this.db.query("PRAGMA user_version").get() as { user_version?: number } | null;
    if (version?.user_version !== SCHEMA_VERSION) throw fatal("session event store schema mismatch");
    this.checkColumns("sequence_state", ["singleton", "reserved_through", "legacy_imported"]);
    this.checkColumns("lifecycle_events", ["client_seq", "kind", "event_json", "payload_bytes"]);
    const stateCount = this.db.query("SELECT COUNT(*) AS n FROM sequence_state WHERE singleton=1").get() as { n: number };
    const totalCount = this.db.query("SELECT COUNT(*) AS n FROM sequence_state").get() as { n: number };
    if (stateCount.n !== 1 || totalCount.n !== 1) throw fatal("session event store sequence state mismatch");
  }
  private checkColumns(table: string, expected: string[]): void {
    const rows = this.db.query(`PRAGMA table_info(${table})`).all() as Array<{ name: string }>;
    if (rows.length !== expected.length || rows.some((row, i) => row.name !== expected[i])) throw fatal("session event store schema mismatch");
  }
  private enforcePageLimit(): void {
    const page = this.db.query("PRAGMA page_size").get() as { page_size?: number } | null;
    if (!page?.page_size || !Number.isSafeInteger(page.page_size)) throw fatal("session event store page size is invalid");
    const maxPages = Math.floor(SESSION_EVENT_STORE_MAX_DATABASE_BYTES / page.page_size);
    const set = this.db.query(`PRAGMA max_page_count=${maxPages}`).get() as { max_page_count?: number } | null;
    const count = this.db.query("PRAGMA page_count").get() as { page_count?: number } | null;
    if (set?.max_page_count !== maxPages || !Number.isSafeInteger(count?.page_count) || count!.page_count! > maxPages) {
      throw fatal("session event store database exceeds size limit");
    }
  }
  private loadRows(): void {
    let bytes = 0;
    let max = 0;
    const rows = this.db.query("SELECT client_seq,kind,event_json,payload_bytes FROM lifecycle_events ORDER BY client_seq").all() as StoredRow[];
    if (rows.length > SESSION_EVENT_STORE_MAX_ROWS) throw fatal("session event store row limit exceeded");
    for (const row of rows) { this.validateRow(row); bytes += row.payload_bytes; max = Math.max(max, row.client_seq); }
    if (!Number.isSafeInteger(bytes) || bytes > SESSION_EVENT_STORE_MAX_PAYLOAD_BYTES) throw fatal("session event store payload limit exceeded");
    if (max > this.readState().reserved_through) throw fatal("session event store sequence state is unsafe");
    this.pendingRows = rows.length; this.pendingBytes = bytes;
  }
  private validateRow(row: StoredRow): StoredLifecycleEvent {
    const kind = durableKind(row.kind);
    if (!Number.isSafeInteger(row.client_seq) || row.client_seq <= 0 || !kind || typeof row.event_json !== "string" ||
        !Number.isSafeInteger(row.payload_bytes) || row.payload_bytes <= 0 || byteLength(row.event_json) !== row.payload_bytes) {
      throw fatal("session event store contains a corrupt event");
    }
    let decoded: unknown;
    try { decoded = JSON.parse(row.event_json); }
    catch (error) { throw fatal("session event store contains a corrupt event", error); }
    const parsed = SessionEvent.safeParse(decoded);
    if (!parsed.success || parsed.data.kind !== kind || !durableKind(parsed.data.kind)) throw fatal("session event store contains a corrupt event");
    return { clientSeq: row.client_seq, kind, event: parsed.data, payloadBytes: row.payload_bytes };
  }
  private reserveInitialBlock(): void {
    const before = this.readState();
    const legacy = before.legacy_imported === 0 ? readLegacySequence(this.legacySequencePath) : null;
    const floor = Math.max(before.reserved_through, legacy ?? 0);
    const end = this.blockEnd(floor);
    this.transaction("sequence reservation", () => {
      const current = this.readState();
      if (current.reserved_through !== before.reserved_through || current.legacy_imported !== before.legacy_imported) throw fatal("session event store sequence state changed concurrently");
      this.db.query("UPDATE sequence_state SET reserved_through=?,legacy_imported=1 WHERE singleton=1").run(end);
    });
    this.currentSequence = floor; this.reservedThrough = end;
    if (before.legacy_imported === 0 && legacy !== null) { try { unlinkSync(this.legacySequencePath); } catch { /* best effort */ } }
  }
  private reserveNextBlock(): void {
    const before = this.readState();
    if (before.legacy_imported !== 1) throw fatal("session event store sequence state is unsafe");
    const floor = Math.max(this.currentSequence, before.reserved_through);
    const end = this.blockEnd(floor);
    this.transaction("sequence reservation", () => {
      const current = this.readState();
      if (current.reserved_through !== before.reserved_through || current.legacy_imported !== 1) throw fatal("session event store sequence state changed concurrently");
      this.db.query("UPDATE sequence_state SET reserved_through=? WHERE singleton=1").run(end);
    });
    this.currentSequence = floor; this.reservedThrough = end;
  }
  private blockEnd(floor: number): number {
    if (!safeSeq(floor) || floor > MAX_SEQ - SESSION_EVENT_SEQUENCE_BLOCK_SIZE) throw fatal("session event store sequence exhausted");
    return floor + SESSION_EVENT_SEQUENCE_BLOCK_SIZE;
  }
  private readState(): { reserved_through: number; legacy_imported: number } {
    const row = this.db.query("SELECT reserved_through,legacy_imported FROM sequence_state WHERE singleton=1").get() as { reserved_through: number; legacy_imported: number } | null;
    if (!row || !safeSeq(row.reserved_through) || (row.legacy_imported !== 0 && row.legacy_imported !== 1)) throw fatal("session event store sequence state is unsafe");
    return row;
  }
  private sqliteFull(error: unknown): boolean {
    const code = error && typeof error === "object" ? (error as { code?: unknown }).code : undefined;
    return code === "SQLITE_FULL" || code === 13;
  }
}
interface StoredRow { client_seq: number; kind: string; event_json: string; payload_bytes: number }
export function openSessionEventStore(options: SessionEventStoreOptions = {}): SessionEventStore {
  return new SessionEventStore(options);
}
