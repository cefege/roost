// Pins durable session-event storage, exact acknowledgements, and v1 migration.
// Tests exercise SQLite rows directly where startup must fail closed rather
// than silently dropping a pending event or rewinding its sequence.
import { afterEach, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { SessionEvent } from "@roost/shared/wire";
import {
  SESSION_EVENT_SEQUENCE_BLOCK_SIZE,
  SESSION_EVENT_STORE_MAX_PAYLOAD_BYTES,
  SESSION_EVENT_STORE_MAX_ROWS,
  SessionEventStore,
  SessionEventStoreFatalError,
  SessionEventOutboxFullError,
  type SessionEventReservation,
} from "../src/transport/session-event-store.ts";

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function paths(): { root: string; dbPath: string; legacySequencePath: string } {
  const root = mkdtempSync(join(tmpdir(), "roost-session-event-store-"));
  roots.push(root);
  return {
    root,
    dbPath: join(root, "session-event-outbox.sqlite"),
    legacySequencePath: join(root, "client-seq.txt"),
  };
}

function opened(cwd = "/tmp"): SessionEvent {
  return {
    kind: "opened",
    ts: 1,
    session_id: "00000000-0000-4000-8000-000000000001" as never,
    worker_fp: "a".repeat(64) as never,
    channel: 1 as never,
    session_kind: "shell",
    cwd,
  };
}

function agentReference(value: string | null): SessionEvent {
  return {
    kind: "agent_reference",
    ts: 2,
    session_id: "00000000-0000-4000-8000-000000000001" as never,
    reference: value === null
      ? null
      : {
          schema_version: 1,
          agent_id: "omp",
          kind: "path",
          value,
        },
  };
}

function createLegacyStore(
  options: ReturnType<typeof paths>,
  rows: ReadonlyArray<{ clientSeq: number; event: SessionEvent }>,
): void {
  const db = new Database(options.dbPath, { create: true });
  db.exec(`
    CREATE TABLE sequence_state (
      singleton INTEGER PRIMARY KEY CHECK(singleton=1),
      reserved_through INTEGER NOT NULL CHECK(reserved_through BETWEEN 0 AND 9007199254740991),
      legacy_imported INTEGER NOT NULL CHECK(legacy_imported IN (0,1))
    ) STRICT;
    INSERT INTO sequence_state VALUES (1,200,1);
    CREATE TABLE lifecycle_events (
      client_seq INTEGER PRIMARY KEY CHECK(client_seq > 0 AND client_seq <= 9007199254740991),
      kind TEXT NOT NULL CHECK(kind IN ('opened','closed','respawned')),
      event_json TEXT NOT NULL,
      payload_bytes INTEGER NOT NULL CHECK(payload_bytes > 0 AND payload_bytes <= 8388608)
    ) STRICT;
    PRAGMA user_version=1;
  `);
  const insert = db.query(
    "INSERT INTO lifecycle_events (client_seq,kind,event_json,payload_bytes) VALUES (?,?,?,?)",
  );
  for (const row of rows) {
    const json = JSON.stringify(row.event);
    insert.run(row.clientSeq, row.event.kind, json, Buffer.byteLength(json));
  }
  db.close();
}

test("durable session-event append reopens and only exact ACK removes it", () => {
  const options = paths();
  const first = new SessionEventStore(options);
  const reservation = first.reserveSessionEvent("opened");
  const stored = first.appendSessionEvent(reservation, opened());
  const transientSeq = first.nextClientSeq();
  expect(stored.clientSeq).toBeGreaterThan(0);
  expect(transientSeq).toBe(stored.clientSeq + 1);
  first.close();

  const reopened = new SessionEventStore(options);
  expect(reopened.pendingEvents()).toEqual([stored]);
  expect(reopened.acknowledge(stored.clientSeq + 1)).toBe(false);
  expect(reopened.pendingEvents()).toHaveLength(1);
  expect(reopened.acknowledge(stored.clientSeq)).toBe(true);
  expect(reopened.acknowledge(stored.clientSeq)).toBe(false);
  expect(reopened.pendingEvents()).toEqual([]);
  reopened.close();

  const final = new SessionEventStore(options);
  expect(final.pendingEvents()).toEqual([]);
  expect(final.nextClientSeq()).toBeGreaterThan(transientSeq);
  final.close();
});

test("v1 lifecycle rows migrate transactionally without sequence or ACK loss", () => {
  const options = paths();
  const firstEvent = opened("/first");
  const secondEvent = opened("/second");
  createLegacyStore(options, [
    { clientSeq: 4, event: firstEvent },
    { clientSeq: 99, event: secondEvent },
  ]);

  const migrated = new SessionEventStore(options);
  expect(migrated.pendingEvents().map(({ clientSeq, event }) => ({
    clientSeq,
    event,
  }))).toEqual([
    { clientSeq: 4, event: firstEvent },
    { clientSeq: 99, event: secondEvent },
  ]);
  expect(migrated.acknowledge(4)).toBe(true);
  expect(migrated.nextClientSeq()).toBe(201);
  migrated.close();

  const db = new Database(options.dbPath);
  expect((db.query("PRAGMA user_version").get() as { user_version: number }).user_version).toBe(2);
  expect((db.query(
    "SELECT GROUP_CONCAT(name, ',') AS names FROM (SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name)",
  ).get() as { names: string }).names).toBe("sequence_state,session_events");
  db.close();

  const reopened = new SessionEventStore(options);
  expect(reopened.pendingEvents().map(({ clientSeq }) => clientSeq)).toEqual([99]);
  expect(reopened.nextClientSeq()).toBeGreaterThan(201);
  reopened.close();
});

test("agent references persist set, replace, and clear within the private event bound", () => {
  const options = paths();
  const store = new SessionEventStore(options);
  const events = [
    agentReference("/tmp/session-one.jsonl"),
    agentReference("/tmp/session-two.jsonl"),
    agentReference(null),
  ];
  for (const event of events) {
    store.appendSessionEvent(store.reserveSessionEvent("agent_reference"), event);
  }
  expect(store.pendingEvents().map(({ event }) => event)).toEqual(events);

  const oversized = store.reserveSessionEvent("agent_reference");
  expect(() => store.appendSessionEvent(
    oversized,
    agentReference("x".repeat(4_097)),
  )).toThrow(SessionEventStoreFatalError);
  store.releaseSessionEvent(oversized);
  expect(() => store.reserveSessionEvent("agent_reference", 8_193)).toThrow(
    SessionEventStoreFatalError,
  );
  store.close();

  const reopened = new SessionEventStore(options);
  expect(reopened.pendingEvents().map(({ event }) => event)).toEqual(events);
  reopened.close();
});

test("legacy watermark is imported once as a sequence floor", () => {
  const options = paths();
  writeFileSync(options.legacySequencePath, "4096\n");
  const first = new SessionEventStore(options);
  expect(first.nextClientSeq()).toBe(4097);
  first.close();

  // A stale file appearing again cannot rewind or re-floor an initialized DB.
  writeFileSync(options.legacySequencePath, "1\n");
  const reopened = new SessionEventStore(options);
  expect(reopened.nextClientSeq()).toBeGreaterThan(4097);
  reopened.close();
});

test("sequence allocation reserves a new 1024-value block without reuse", () => {
  const options = paths();
  const first = new SessionEventStore(options);
  let sequence = 0;
  for (let i = 0; i <= SESSION_EVENT_SEQUENCE_BLOCK_SIZE; i++) sequence = first.nextClientSeq();
  expect(sequence).toBe(SESSION_EVENT_SEQUENCE_BLOCK_SIZE + 1);
  first.close();

  const reopened = new SessionEventStore(options);
  expect(reopened.nextClientSeq()).toBeGreaterThan(sequence);
  reopened.close();
});

test("row and payload reservations are bounded and exactly released", () => {
  const options = paths();
  const store = new SessionEventStore(options);
  const heldClose = store.reserveSessionEvent("closed", 1);
  expect(store.stats()).toMatchObject({
    reservedRows: 1,
    blockingReservedRows: 1,
  });
  store.holdSessionEvent(heldClose);
  expect(store.stats()).toMatchObject({
    reservedRows: 1,
    blockingReservedRows: 0,
  });
  store.releaseSessionEvent(heldClose);
  const reservations: SessionEventReservation[] = [];
  for (let i = 0; i < SESSION_EVENT_STORE_MAX_ROWS; i++) {
    reservations.push(store.reserveSessionEvent("closed", 1));
  }
  expect(() => store.reserveSessionEvent("closed", 1)).toThrow(SessionEventOutboxFullError);
  for (const reservation of reservations) store.releaseSessionEvent(reservation);
  expect(store.stats()).toEqual({
    pendingRows: 0,
    pendingBytes: 0,
    reservedRows: 0,
    reservedBytes: 0,
    blockingReservedRows: 0,
  });

  const allBytes = store.reserveSessionEvent("opened", SESSION_EVENT_STORE_MAX_PAYLOAD_BYTES);
  expect(() => store.reserveSessionEvent("closed", 1)).toThrow("session event outbox full");
  store.releaseSessionEvent(allBytes);
  expect(() => store.releaseSessionEvent(allBytes)).toThrow(SessionEventStoreFatalError);
  store.close();
});

test("schema mismatch and corrupt durable rows fail closed", () => {
  const mismatched = paths();
  const wrong = new Database(mismatched.dbPath, { create: true });
  wrong.exec("CREATE TABLE unexpected (id INTEGER PRIMARY KEY); PRAGMA user_version=1");
  wrong.close();
  expect(() => new SessionEventStore(mismatched)).toThrow(SessionEventStoreFatalError);

  const corrupt = paths();
  const valid = new SessionEventStore(corrupt);
  valid.close();
  const db = new Database(corrupt.dbPath);
  db.query("INSERT INTO session_events (client_seq,kind,event_json,payload_bytes) VALUES (?,?,?,?)")
    .run(1, "opened", "x", 1);
  db.close();
  expect(() => new SessionEventStore(corrupt)).toThrow("session event store contains a corrupt event");
});

test("startup rejects substituted constraints and unexpected schema objects", () => {
  const substituted = paths();
  const substitutedDb = new Database(substituted.dbPath, { create: true });
  substitutedDb.exec(`
    CREATE TABLE sequence_state (
      singleton INTEGER PRIMARY KEY CHECK(singleton=1),
      reserved_through INTEGER NOT NULL CHECK(reserved_through BETWEEN 0 AND 9007199254740991),
      legacy_imported INTEGER NOT NULL CHECK(legacy_imported IN (0,1))
    ) STRICT;
    INSERT INTO sequence_state VALUES (1,0,1);
    CREATE TABLE session_events (
      client_seq INTEGER PRIMARY KEY,
      kind TEXT NOT NULL,
      event_json TEXT NOT NULL,
      payload_bytes INTEGER NOT NULL
    ) STRICT;
    PRAGMA user_version=2;
  `);
  substitutedDb.close();
  expect(() => new SessionEventStore(substituted)).toThrow(
    "session event store schema mismatch",
  );

  const unexpectedObjects = [
    "CREATE VIEW session_event_sequences AS SELECT client_seq FROM session_events",
    "CREATE INDEX session_event_kind_index ON session_events(kind)",
    `CREATE TRIGGER delete_inserted_event
       AFTER INSERT ON session_events
       BEGIN
         DELETE FROM session_events WHERE client_seq=NEW.client_seq;
       END`,
  ];
  for (const objectSql of unexpectedObjects) {
    const options = paths();
    const initialized = new SessionEventStore(options);
    initialized.close();
    const alteredDb = new Database(options.dbPath);
    alteredDb.exec(objectSql);
    alteredDb.close();
    expect(() => new SessionEventStore(options)).toThrow(
      "session event store schema mismatch",
    );
  }
});

test("append rolls back when a new trigger removes the inserted row", () => {
  const options = paths();
  const store = new SessionEventStore(options);
  const reservation = store.reserveSessionEvent("opened");
  const alteredDb = new Database(options.dbPath);
  alteredDb.exec(`
    CREATE TRIGGER delete_inserted_event
    AFTER INSERT ON session_events
    BEGIN
      DELETE FROM session_events WHERE client_seq=NEW.client_seq;
    END;
  `);
  alteredDb.close();

  expect(() => store.appendSessionEvent(reservation, opened())).toThrow(
    "session event store append verification failed",
  );
  expect(store.pendingEvents()).toEqual([]);
  expect(store.stats()).toMatchObject({
    pendingRows: 0,
    pendingBytes: 0,
    reservedRows: 1,
  });
  store.releaseSessionEvent(reservation);
  store.close();
});

test("a corrupt v1 row leaves the legacy schema untouched", () => {
  const options = paths();
  createLegacyStore(options, [{ clientSeq: 4, event: opened() }]);
  const db = new Database(options.dbPath);
  db.query(
    "UPDATE lifecycle_events SET event_json='not-json', payload_bytes=8 WHERE client_seq=4",
  ).run();
  db.close();

  expect(() => new SessionEventStore(options)).toThrow(
    "session event store contains a corrupt event",
  );
  const unchanged = new Database(options.dbPath);
  expect((unchanged.query("PRAGMA user_version").get() as {
    user_version: number;
  }).user_version).toBe(1);
  expect((unchanged.query(
    "SELECT COUNT(*) AS n FROM lifecycle_events",
  ).get() as { n: number }).n).toBe(1);
  unchanged.close();
});

test("malformed and unsafe legacy watermarks are fatal", () => {
  const malformed = paths();
  writeFileSync(malformed.legacySequencePath, "not-a-sequence");
  expect(() => new SessionEventStore(malformed)).toThrow(SessionEventStoreFatalError);

  const exhausted = paths();
  writeFileSync(exhausted.legacySequencePath, String(Number.MAX_SAFE_INTEGER));
  expect(() => new SessionEventStore(exhausted)).toThrow("session event store sequence exhausted");
});
