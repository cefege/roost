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
  SessionLifecycleOutboxFullError,
  type LifecycleReservation,
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

test("durable lifecycle append reopens and only exact ACK removes it", () => {
  const options = paths();
  const first = new SessionEventStore(options);
  const reservation = first.reserveLifecycleEvent("opened");
  const stored = first.appendLifecycleEvent(reservation, opened());
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
  const heldClose = store.reserveLifecycleEvent("closed", 1);
  expect(store.stats()).toMatchObject({
    reservedRows: 1,
    blockingReservedRows: 1,
  });
  store.holdLifecycleEvent(heldClose);
  expect(store.stats()).toMatchObject({
    reservedRows: 1,
    blockingReservedRows: 0,
  });
  store.releaseLifecycleEvent(heldClose);
  const reservations: LifecycleReservation[] = [];
  for (let i = 0; i < SESSION_EVENT_STORE_MAX_ROWS; i++) {
    reservations.push(store.reserveLifecycleEvent("closed", 1));
  }
  expect(() => store.reserveLifecycleEvent("closed", 1)).toThrow(SessionLifecycleOutboxFullError);
  for (const reservation of reservations) store.releaseLifecycleEvent(reservation);
  expect(store.stats()).toEqual({
    pendingRows: 0,
    pendingBytes: 0,
    reservedRows: 0,
    reservedBytes: 0,
    blockingReservedRows: 0,
  });

  const allBytes = store.reserveLifecycleEvent("opened", SESSION_EVENT_STORE_MAX_PAYLOAD_BYTES);
  expect(() => store.reserveLifecycleEvent("closed", 1)).toThrow("session lifecycle outbox full");
  store.releaseLifecycleEvent(allBytes);
  expect(() => store.releaseLifecycleEvent(allBytes)).toThrow(SessionEventStoreFatalError);
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
  db.query("INSERT INTO lifecycle_events (client_seq,kind,event_json,payload_bytes) VALUES (?,?,?,?)")
    .run(1, "opened", "x", 1);
  db.close();
  expect(() => new SessionEventStore(corrupt)).toThrow("session event store contains a corrupt event");
});

test("malformed and unsafe legacy watermarks are fatal", () => {
  const malformed = paths();
  writeFileSync(malformed.legacySequencePath, "not-a-sequence");
  expect(() => new SessionEventStore(malformed)).toThrow(SessionEventStoreFatalError);

  const exhausted = paths();
  writeFileSync(exhausted.legacySequencePath, String(Number.MAX_SAFE_INTEGER));
  expect(() => new SessionEventStore(exhausted)).toThrow("session event store sequence exhausted");
});
