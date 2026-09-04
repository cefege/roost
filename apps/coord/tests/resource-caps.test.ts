/**
 * Covers persistence caps that keep coordinator event storage bounded and valid.
 * Bun discovers this suite directly and writes events through the production log.
 * Its cases depend on shared wire shapes and a migrated temporary SQLite database.
 */
import {
  afterAll,
  beforeAll,
  describe,
  expect,
  test,
} from "bun:test";
import {
  SessionEvent,
  asChannelId,
  asSessionId,
  asWorkerFp,
  type Session,
} from "@roost/shared/wire";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  openDb,
  type DbHandle,
  type KyselyDB,
} from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { appendEvent } from "../src/event-log.ts";
import {
  MAX_PERSISTED_UTF8_BYTES,
  MAX_WORKER_SNAPSHOT_SESSIONS,
  truncatePersistedUtf8,
} from "../src/persistence-input.ts";

const WORKER_FP = asWorkerFp("d".repeat(64));
const DASHBOARD_ID = "resource-caps-dashboard";
const ORGANIZATION_ID = "resource-caps-organization";
const SESSION_ID = asSessionId("44444444-4444-4444-8444-444444444444");

let workdir: string;
let eventDb: DbHandle;
let db: KyselyDB;
let clientSeq = 0;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-resource-caps-"));
  eventDb = openDb(join(workdir, "events.db"));
  db = eventDb.db;
  await runMigrations(eventDb.sqlite);
  const now = Date.now();
  await db.insertInto("organizations").values({
    id: ORGANIZATION_ID,
    slug: "resource-caps",
    name: "Resource caps",
    status: "active",
    created_at_ms: now,
  }).execute();
  await db.insertInto("dashboards").values({
    id: DASHBOARD_ID,
    organization_id: ORGANIZATION_ID,
    slug: "resource-caps",
    name: "Resource caps",
    status: "active",
    created_at_ms: now,
  }).execute();
  await db.insertInto("workers").values({
    fp: WORKER_FP,
    dashboard_id: DASHBOARD_ID,
    label: "bounded-worker",
    os: "linux",
    git_sha: null,
    host_metrics_json: null,
    registered_at_ms: now,
    last_seen_ms: now,
  }).execute();
});

afterAll(async () => {
  try {
    await eventDb?.close();
  } finally {
    if (workdir && existsSync(workdir)) {
      rmSync(workdir, { recursive: true, force: true });
    }
  }
});

function utf8Bytes(value: string): number {
  return Buffer.byteLength(value, "utf8");
}

async function persistWorkerEvent(event: SessionEvent): Promise<void> {
  clientSeq += 1;
  await appendEvent(db, event, {
    worker_fp: WORKER_FP,
    client_seq: clientSeq,
    dashboardId: DASHBOARD_ID,
  });
}

function oversized(prefix: string): string {
  return `${prefix}${"😀".repeat(2_000)}`;
}

describe("UTF-8 persistence input", () => {
  test("keeps exact byte boundaries and never splits a scalar value", () => {
    const exactAscii = "a".repeat(MAX_PERSISTED_UTF8_BYTES);
    expect(truncatePersistedUtf8(exactAscii)).toBe(exactAscii);
    expect(truncatePersistedUtf8(`${exactAscii}b`)).toBe(exactAscii);

    const exactUnicode = `${"a".repeat(MAX_PERSISTED_UTF8_BYTES - 4)}😀`;
    expect(utf8Bytes(exactUnicode)).toBe(MAX_PERSISTED_UTF8_BYTES);
    expect(truncatePersistedUtf8(`${exactUnicode}b`)).toBe(exactUnicode);
    expect(truncatePersistedUtf8(exactUnicode, MAX_PERSISTED_UTF8_BYTES - 1))
      .toBe("a".repeat(MAX_PERSISTED_UTF8_BYTES - 4));
    expect(truncatePersistedUtf8("😀", 3)).toBe("");
    expect(truncatePersistedUtf8("😀", 4)).toBe("😀");
  });

  test("event JSON and its projection use the identical bounded strings", async () => {
    const openedCwd = oversized("/opened/");
    await persistWorkerEvent(SessionEvent.parse({
      kind: "opened",
      session_id: SESSION_ID,
      worker_fp: WORKER_FP,
      channel: asChannelId(7),
      session_kind: "shell",
      cwd: openedCwd,
      ts: Date.now(),
    }));

    let row = await db.selectFrom("sessions").selectAll()
      .where("id", "=", SESSION_ID).executeTakeFirstOrThrow();
    let storedEvent = await db.selectFrom("events").select("payload_json")
      .where("client_seq", "=", clientSeq).executeTakeFirstOrThrow();
    let payload = JSON.parse(storedEvent.payload_json) as SessionEvent;
    expect(payload.kind).toBe("opened");
    if (payload.kind !== "opened") throw new Error("expected opened payload");
    expect(payload.cwd).toBe(truncatePersistedUtf8(openedCwd));
    expect(row.cwd).toBe(payload.cwd);
    expect(row.spawn_cwd).toBe(payload.cwd);

    const cwd = oversized("/cwd/");
    await persistWorkerEvent(SessionEvent.parse({
      kind: "cwd", session_id: SESSION_ID, cwd, ts: Date.now(),
    }));
    row = await db.selectFrom("sessions").selectAll()
      .where("id", "=", SESSION_ID).executeTakeFirstOrThrow();
    storedEvent = await db.selectFrom("events").select("payload_json")
      .where("client_seq", "=", clientSeq).executeTakeFirstOrThrow();
    payload = JSON.parse(storedEvent.payload_json) as SessionEvent;
    if (payload.kind !== "cwd") throw new Error("expected cwd payload");
    expect(row.cwd).toBe(payload.cwd);

    const title = oversized("title-");
    await persistWorkerEvent(SessionEvent.parse({
      kind: "renamed", session_id: SESSION_ID, custom_title: title, ts: Date.now(),
    }));
    row = await db.selectFrom("sessions").selectAll()
      .where("id", "=", SESSION_ID).executeTakeFirstOrThrow();
    storedEvent = await db.selectFrom("events").select("payload_json")
      .where("client_seq", "=", clientSeq).executeTakeFirstOrThrow();
    payload = JSON.parse(storedEvent.payload_json) as SessionEvent;
    if (payload.kind !== "renamed") throw new Error("expected renamed payload");
    expect(row.custom_title).toBe(payload.custom_title);

    const branch = oversized("branch-");
    const remote = oversized("owner/repo-");
    await persistWorkerEvent(SessionEvent.parse({
      kind: "git", session_id: SESSION_ID, branch, remote, ts: Date.now(),
    }));
    row = await db.selectFrom("sessions").selectAll()
      .where("id", "=", SESSION_ID).executeTakeFirstOrThrow();
    storedEvent = await db.selectFrom("events").select("payload_json")
      .where("client_seq", "=", clientSeq).executeTakeFirstOrThrow();
    payload = JSON.parse(storedEvent.payload_json) as SessionEvent;
    if (payload.kind !== "git") throw new Error("expected git payload");
    expect(row.git_branch).toBe(payload.branch);
    expect(row.git_remote).toBe(payload.remote ?? null);

    const url = oversized("https://example.test/");
    await persistWorkerEvent(SessionEvent.parse({
      kind: "pr",
      session_id: SESSION_ID,
      number: 123,
      state: "open",
      checks: "passing",
      url,
      ts: Date.now(),
    }));
    row = await db.selectFrom("sessions").selectAll()
      .where("id", "=", SESSION_ID).executeTakeFirstOrThrow();
    storedEvent = await db.selectFrom("events").select("payload_json")
      .where("client_seq", "=", clientSeq).executeTakeFirstOrThrow();
    payload = JSON.parse(storedEvent.payload_json) as SessionEvent;
    if (payload.kind !== "pr") throw new Error("expected pr payload");
    expect(row.pr_url).toBe(payload.url);

    for (const value of [
      row.cwd,
      row.spawn_cwd,
      row.custom_title,
      row.git_branch,
      row.git_remote,
      row.pr_url,
    ]) {
      expect(value).not.toBeNull();
      expect(utf8Bytes(value!)).toBeLessThanOrEqual(MAX_PERSISTED_UTF8_BYTES);
    }
  });

  test("snapshot session strings are bounded before JSON and projection", async () => {
    const value = oversized("snapshot-");
    const session: Session = {
      id: asSessionId("55555555-5555-4555-8555-555555555555"),
      worker_fp: WORKER_FP,
      channel: asChannelId(9),
      kind: "shell",
      cwd: value,
      spawn_cwd: value,
      workspace_id: null,
      status: "open",
      created_at: Date.now(),
      closed_at: null,
      custom_title: value,
      git_branch: value,
      git_remote: value,
      pr_number: 9,
      pr_state: "open",
      pr_checks: "pending",
      pr_url: value,
      ports: [],
    };
    await persistWorkerEvent(SessionEvent.parse({
      kind: "snapshot",
      worker_fp: WORKER_FP,
      sessions: [session],
      ts: Date.now(),
    }));
    const row = await db.selectFrom("sessions").selectAll()
      .where("id", "=", session.id).executeTakeFirstOrThrow();
    const stored = await db.selectFrom("events").select("payload_json")
      .where("client_seq", "=", clientSeq).executeTakeFirstOrThrow();
    const payload = JSON.parse(stored.payload_json) as SessionEvent;
    if (payload.kind !== "snapshot") throw new Error("expected snapshot payload");
    const [persisted] = payload.sessions;
    if (persisted === undefined) throw new Error("expected persisted snapshot session");
    expect(row.cwd).toBe(persisted.cwd);
    expect(row.spawn_cwd).toBe(persisted.spawn_cwd ?? null);
    expect(row.custom_title).toBe(persisted.custom_title);
    expect(row.git_branch).toBe(persisted.git_branch ?? null);
    expect(row.git_remote).toBe(persisted.git_remote ?? null);
    expect(row.pr_url).toBe(persisted.pr_url ?? null);
    expect(persisted.cwd).toBe(truncatePersistedUtf8(value));
    expect(utf8Bytes(persisted.cwd)).toBeLessThanOrEqual(MAX_PERSISTED_UTF8_BYTES);
  });
});

describe("worker snapshot cap", () => {
  function snapshotSessions(count: number): Session[] {
    return Array.from({ length: count }, (_, index) => ({
      id: asSessionId(
        `60000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`,
      ),
      worker_fp: WORKER_FP,
      channel: asChannelId(index + 1),
      kind: "shell" as const,
      cwd: "/tmp",
      workspace_id: null,
      status: "open" as const,
      created_at: 1,
      closed_at: null,
      custom_title: null,
    }));
  }

  test("accepts exactly 1,024 sessions", async () => {
    const sessions = snapshotSessions(MAX_WORKER_SNAPSHOT_SESSIONS);
    await persistWorkerEvent(SessionEvent.parse({
      kind: "snapshot",
      worker_fp: WORKER_FP,
      sessions,
      ts: Date.now(),
    }));
    const count = await db.selectFrom("sessions")
      .select(({ fn }) => fn.countAll<number>().as("count"))
      .where("worker_fp", "=", WORKER_FP)
      .executeTakeFirstOrThrow();
    expect(Number(count.count)).toBeGreaterThanOrEqual(MAX_WORKER_SNAPSHOT_SESSIONS);
  });

  test("rejects 1,025 sessions before touching SQLite", async () => {
    let touched = false;
    const untouchedDb = new Proxy({}, {
      get() {
        touched = true;
        throw new Error("database touched");
      },
    }) as KyselyDB;
    const event = SessionEvent.parse({
      kind: "snapshot",
      worker_fp: WORKER_FP,
      sessions: snapshotSessions(MAX_WORKER_SNAPSHOT_SESSIONS + 1),
      ts: Date.now(),
    });
    await expect(appendEvent(untouchedDb, event, {
      worker_fp: WORKER_FP,
      client_seq: 1,
      dashboardId: DASHBOARD_ID,
    })).rejects.toThrow("worker snapshot exceeds 1024 sessions");
    expect(touched).toBe(false);
  });
});
