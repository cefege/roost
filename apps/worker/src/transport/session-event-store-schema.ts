// Owns the exact durable worker session-event SQLite schema and its migration.
// SessionEventStore accepts only canonical v1/v2 objects before reading rows;
// validated v1 rows are copied to v2 without changing sequence state.

import type { Database } from "bun:sqlite";
import {
  AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES,
} from "@roost/shared/agent-conversation-reference";
import {
  SessionEvent,
  type SessionEvent as SessionEventValue,
} from "@roost/shared/wire";
import { sessionEventStoreFatal } from "./session-event-store-errors.ts";

export const SESSION_EVENT_STORE_SCHEMA_VERSION = 2;
export const MAX_SESSION_EVENT_SEQUENCE = Number.MAX_SAFE_INTEGER;

export type DurableSessionEventKind =
  | "opened"
  | "closed"
  | "respawned"
  | "agent_reference";

export interface StoredSessionEventRow {
  client_seq: number;
  kind: string;
  event_json: string;
  payload_bytes: number;
}

export interface DecodedStoredSessionEvent {
  readonly clientSeq: number;
  readonly kind: DurableSessionEventKind;
  readonly event: SessionEventValue;
  readonly payloadBytes: number;
}

interface RowLimits {
  maxRows: number;
  maxPayloadBytes: number;
}

type TransactionRunner = (label: string, body: () => void) => void;

const encoder = new TextEncoder();
const V1_TABLES = "lifecycle_events,sequence_state";
const V2_TABLES = "sequence_state,session_events";
const EVENT_COLUMNS = ["client_seq", "kind", "event_json", "payload_bytes"] as const;
const STATE_COLUMNS = ["singleton", "reserved_through", "legacy_imported"] as const;
const SEQUENCE_STATE_TABLE_SQL = `
  CREATE TABLE sequence_state (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    reserved_through INTEGER NOT NULL CHECK(reserved_through BETWEEN 0 AND ${MAX_SESSION_EVENT_SEQUENCE}),
    legacy_imported INTEGER NOT NULL CHECK(legacy_imported IN (0,1))
  ) STRICT`;

interface StoredSchemaObject {
  type: string;
  name: string;
  table_name: string;
  sql: string | null;
}

export function durableSessionEventKind(
  value: unknown,
): DurableSessionEventKind | null {
  return value === "opened" || value === "closed" || value === "respawned" ||
      value === "agent_reference"
    ? value
    : null;
}

export function serializedSessionEventLimit(kind: DurableSessionEventKind): number {
  return kind === "agent_reference"
    ? AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES
    : Number.MAX_SAFE_INTEGER;
}

export function decodeStoredSessionEventRow(
  row: StoredSessionEventRow,
): DecodedStoredSessionEvent {
  const kind = durableSessionEventKind(row.kind);
  if (
    !Number.isSafeInteger(row.client_seq) || row.client_seq <= 0 || !kind ||
    typeof row.event_json !== "string" ||
    !Number.isSafeInteger(row.payload_bytes) || row.payload_bytes <= 0 ||
    encoder.encode(row.event_json).byteLength !== row.payload_bytes ||
    row.payload_bytes > serializedSessionEventLimit(kind)
  ) {
    throw sessionEventStoreFatal("session event store contains a corrupt event");
  }
  let decoded: unknown;
  try {
    decoded = JSON.parse(row.event_json);
  } catch (error) {
    throw sessionEventStoreFatal(
      "session event store contains a corrupt event",
      error,
    );
  }
  const parsed = SessionEvent.safeParse(decoded);
  if (
    !parsed.success || parsed.data.kind !== kind ||
    !durableSessionEventKind(parsed.data.kind)
  ) {
    throw sessionEventStoreFatal("session event store contains a corrupt event");
  }
  return {
    clientSeq: row.client_seq,
    kind,
    event: parsed.data,
    payloadBytes: row.payload_bytes,
  };
}

export function ensureSessionEventStoreSchema(
  db: Database,
  transaction: TransactionRunner,
  limits: RowLimits,
): void {
  const initialSchema = readSchemaObjects(db);
  if (initialSchema.length === 0) {
    transaction("schema creation", () => createV2Schema(db, limits.maxPayloadBytes));
  } else {
    const version = userVersion(db);
    const tableList = initialSchema
      .filter(({ type, name }) => type === "table" && !name.startsWith("sqlite_"))
      .map(({ name }) => name)
      .sort()
      .join(",");
    if (version === 1 && tableList === V1_TABLES) {
      validateKnownSchema(db, "lifecycle_events", limits.maxPayloadBytes);
      validatePersistedRows(db, "lifecycle_events", limits, false);
      migrateV1ToV2(db, transaction, limits.maxPayloadBytes);
    } else if (version !== SESSION_EVENT_STORE_SCHEMA_VERSION || tableList !== V2_TABLES) {
      throw sessionEventStoreFatal("session event store schema mismatch");
    }
  }
  validateKnownSchema(db, "session_events", limits.maxPayloadBytes);
}

function readSchemaObjects(db: Database): StoredSchemaObject[] {
  return db.query(
    "SELECT type,name,tbl_name AS table_name,sql FROM sqlite_master ORDER BY type,name",
  ).all() as StoredSchemaObject[];
}

function userVersion(db: Database): number | undefined {
  return (db.query("PRAGMA user_version").get() as {
    user_version?: number;
  } | null)?.user_version;
}

function checkColumns(
  db: Database,
  table: "sequence_state" | "lifecycle_events" | "session_events",
  expected: readonly string[],
): void {
  const rows = db.query(`PRAGMA table_info(${table})`).all() as Array<{
    name: string;
  }>;
  if (
    rows.length !== expected.length ||
    rows.some((row, index) => row.name !== expected[index])
  ) {
    throw sessionEventStoreFatal("session event store schema mismatch");
  }
}

function validateKnownSchema(
  db: Database,
  eventTable: "lifecycle_events" | "session_events",
  maxPayloadBytes: number,
): void {
  validateSchemaObjects(db, eventTable, maxPayloadBytes);
  checkColumns(db, "sequence_state", STATE_COLUMNS);
  checkColumns(db, eventTable, EVENT_COLUMNS);
  const stateCount = db.query(
    "SELECT COUNT(*) AS n FROM sequence_state WHERE singleton=1",
  ).get() as { n: number };
  const totalCount = db.query("SELECT COUNT(*) AS n FROM sequence_state").get() as {
    n: number;
  };
  if (stateCount.n !== 1 || totalCount.n !== 1) {
    throw sessionEventStoreFatal("session event store sequence state mismatch");
  }
}

function validateSchemaObjects(
  db: Database,
  eventTable: "lifecycle_events" | "session_events",
  maxPayloadBytes: number,
): void {
  const expectedEventTableSql = eventTableSql(eventTable, maxPayloadBytes);
  const seenTables = new Set<string>();
  for (const object of readSchemaObjects(db)) {
    if (isLegitimateAutoindex(object, eventTable)) continue;
    let expectedSql: string | undefined;
    if (object.name === "sequence_state") {
      expectedSql = SEQUENCE_STATE_TABLE_SQL;
    } else if (object.name === eventTable) {
      expectedSql = expectedEventTableSql;
    }
    const normalizedActualSql = typeof object.sql === "string"
      ? object.sql.replace(/\s+/g, " ").trim()
      : null;
    const normalizedExpectedSql = expectedSql?.replace(/\s+/g, " ").trim();
    if (
      object.type !== "table" || object.table_name !== object.name ||
      expectedSql === undefined || normalizedActualSql !== normalizedExpectedSql ||
      seenTables.has(object.name)
    ) {
      throw sessionEventStoreFatal("session event store schema mismatch");
    }
    seenTables.add(object.name);
  }
  if (seenTables.size !== 2) {
    throw sessionEventStoreFatal("session event store schema mismatch");
  }
}

function isLegitimateAutoindex(
  object: StoredSchemaObject,
  eventTable: "lifecycle_events" | "session_events",
): boolean {
  if (
    object.type !== "index" || object.sql !== null ||
    (object.table_name !== "sequence_state" && object.table_name !== eventTable)
  ) {
    return false;
  }
  const prefix = `sqlite_autoindex_${object.table_name}_`;
  return object.name.startsWith(prefix) &&
    /^[1-9][0-9]*$/.test(object.name.slice(prefix.length));
}

function eventTableSql(
  table: "lifecycle_events" | "session_events",
  maxPayloadBytes: number,
): string {
  const kinds = table === "session_events"
    ? "'opened','closed','respawned','agent_reference'"
    : "'opened','closed','respawned'";
  return `
    CREATE TABLE ${table} (
      client_seq INTEGER PRIMARY KEY CHECK(client_seq > 0 AND client_seq <= ${MAX_SESSION_EVENT_SEQUENCE}),
      kind TEXT NOT NULL CHECK(kind IN (${kinds})),
      event_json TEXT NOT NULL,
      payload_bytes INTEGER NOT NULL CHECK(payload_bytes > 0 AND payload_bytes <= ${maxPayloadBytes})
    ) STRICT`;
}

function validatePersistedRows(
  db: Database,
  table: "lifecycle_events" | "session_events",
  limits: RowLimits,
  allowReference: boolean,
): { count: number; bytes: number } {
  const rows = db.query(
    `SELECT client_seq,kind,event_json,payload_bytes FROM ${table} ORDER BY client_seq`,
  ).all() as StoredSessionEventRow[];
  if (rows.length > limits.maxRows) {
    throw sessionEventStoreFatal("session event store row limit exceeded");
  }
  let bytes = 0;
  for (const row of rows) {
    const decoded = decodeStoredSessionEventRow(row);
    if (!allowReference && decoded.kind === "agent_reference") {
      throw sessionEventStoreFatal("session event store contains a corrupt event");
    }
    bytes += decoded.payloadBytes;
  }
  if (!Number.isSafeInteger(bytes) || bytes > limits.maxPayloadBytes) {
    throw sessionEventStoreFatal("session event store payload limit exceeded");
  }
  return { count: rows.length, bytes };
}

function createSequenceState(db: Database): void {
  db.exec(`
    ${SEQUENCE_STATE_TABLE_SQL};
    INSERT INTO sequence_state VALUES (1,0,0);
  `);
}

function createSessionEventsTable(db: Database, maxPayloadBytes: number): void {
  db.exec(`${eventTableSql("session_events", maxPayloadBytes)};`);
}

function createV2Schema(db: Database, maxPayloadBytes: number): void {
  createSequenceState(db);
  createSessionEventsTable(db, maxPayloadBytes);
  db.exec(`PRAGMA user_version=${SESSION_EVENT_STORE_SCHEMA_VERSION}`);
}

function migrateV1ToV2(
  db: Database,
  transaction: TransactionRunner,
  maxPayloadBytes: number,
): void {
  transaction("schema migration", () => {
    const before = db.query(
      "SELECT COUNT(*) AS count, COALESCE(SUM(payload_bytes),0) AS bytes FROM lifecycle_events",
    ).get() as { count: number; bytes: number };
    db.exec("ALTER TABLE lifecycle_events RENAME TO lifecycle_events_v1");
    createSessionEventsTable(db, maxPayloadBytes);
    db.exec(`
      INSERT INTO session_events (client_seq,kind,event_json,payload_bytes)
      SELECT client_seq,kind,event_json,payload_bytes
      FROM lifecycle_events_v1
      ORDER BY client_seq;
    `);
    const after = db.query(
      "SELECT COUNT(*) AS count, COALESCE(SUM(payload_bytes),0) AS bytes FROM session_events",
    ).get() as { count: number; bytes: number };
    if (before.count !== after.count || before.bytes !== after.bytes) {
      throw sessionEventStoreFatal("session event store migration copy mismatch");
    }
    db.exec("DROP TABLE lifecycle_events_v1");
    db.exec(`PRAGMA user_version=${SESSION_EVENT_STORE_SCHEMA_VERSION}`);
  });
}
