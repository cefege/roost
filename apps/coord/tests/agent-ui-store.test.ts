import { afterEach, describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { randomUUID } from "node:crypto";
import { readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import {
  AGENT_UI_FRAME_MAX_BYTES,
  ingestAgentUiFrame,
  replayAgentUiSnapshot,
  sweepAgentUiSnapshotStaging,
  validateAgentUiFrame,
} from "../src/agent-ui-store.ts";

const migration = readFileSync(join(import.meta.dir, "../migrations/0017_agent_ui_frames.sql"), "utf8");

const SESSION_ID = "agent-session";
const NOW = Date.UTC(2026, 6, 30);

let sqlite: Database | undefined;
let sqlitePath: string | undefined;

afterEach(() => {
  sqlite?.close();
  sqlite = undefined;
  if (sqlitePath) {
    rmSync(sqlitePath, { force: true });
    rmSync(`${sqlitePath}-wal`, { force: true });
    rmSync(`${sqlitePath}-shm`, { force: true });
    sqlitePath = undefined;
  }
});

function fixture(onDisk = false): Database {
  sqlitePath = onDisk ? join(tmpdir(), `roost-agent-ui-${randomUUID()}.sqlite`) : undefined;
  sqlite = new Database(sqlitePath ?? ":memory:", { create: true });
  sqlite.exec("PRAGMA foreign_keys=ON");
  if (onDisk) sqlite.exec("PRAGMA journal_mode=WAL");
  sqlite.exec(`
    CREATE TABLE sessions (id TEXT PRIMARY KEY, kind TEXT NOT NULL);
    INSERT INTO sessions (id, kind) VALUES ('${SESSION_ID}', 'agent');
  `);
  sqlite.exec(migration);
  return sqlite;
}

function entry(id: string, model: string) {
  return {
    id,
    parentId: null,
    timestamp: "2026-07-30T12:00:00.000Z",
    type: "model_change",
    model,
  };
}

function state(sessionName: string) {
  return {
    isStreaming: false,
    queuedMessageCount: 0,
    sessionName,
    cwd: "/tmp/project",
    participants: [{ name: "host", role: "host" }],
  };
}

function welcome(entryCount: number, sessionName = "snapshot") {
  return {
    t: "welcome",
    proto: 4,
    header: {
      type: "session",
      id: SESSION_ID,
      timestamp: "2026-07-30T12:00:00.000Z",
      cwd: "/tmp/project",
    },
    state: state(sessionName),
    agents: [],
    entryCount,
  };
}

function ingestOutcome(db: Database, hostFrame: object, snapshotId = "", now = NOW) {
  return ingestAgentUiFrame(
    db,
    validateAgentUiFrame({
      sessionId: SESSION_ID,
      frameJson: JSON.stringify(hostFrame),
      snapshotId,
    }),
    now,
  );
}

function ingest(db: Database, hostFrame: object, snapshotId = "", now = NOW) {
  return ingestOutcome(db, hostFrame, snapshotId, now).result;
}

function replay(db: Database) {
  return [...replayAgentUiSnapshot(db, SESSION_ID)].map((frame) => ({
    ...frame,
    host: JSON.parse(frame.frame_json) as Record<string, unknown>,
  }));
}

function replayEntries(frames: ReturnType<typeof replay>): Array<Record<string, unknown>> {
  const entries: Array<Record<string, unknown>> = [];
  const indexById = new Map<unknown, number>();
  for (const frame of frames) {
    const incoming = frame.host.t === "snapshot-chunk"
      ? frame.host.entries as Array<Record<string, unknown>>
      : frame.host.t === "entry"
        ? [frame.host.entry as Record<string, unknown>]
        : [];
    for (const item of incoming) {
      const index = indexById.get(item.id);
      if (index === undefined) {
        indexById.set(item.id, entries.length);
        entries.push(item);
      } else {
        entries[index] = item;
      }
    }
  }
  return entries;
}

function completeSnapshot(db: Database, snapshotId: string, entries: object[]): void {
  expect(ingest(db, welcome(entries.length), snapshotId)).toBe("snapshot-started");
  expect(ingest(db, { t: "snapshot-chunk", entries, final: true }, snapshotId))
    .toBe("snapshot-committed");
}

describe("durable OMP AgentUiFrame replica", () => {
  test("replays a completed snapshot after the coordinator database is reopened", () => {
    let db = fixture(true);
    completeSnapshot(db, "snapshot-1", [entry("a", "provider/a"), entry("b", "provider/b")]);
    db.close();
    sqlite = undefined;

    db = new Database(sqlitePath!);
    sqlite = db;
    db.exec("PRAGMA journal_mode=WAL");
    const frames = replay(db);

    expect(frames.map((frame) => frame.host.t)).toEqual(["welcome", "snapshot-chunk"]);
    expect(frames.map((frame) => frame.coord_revision)).toEqual([1, 2]);
    expect(frames[0]?.snapshot_id).toBe("snapshot-1");
    expect(frames[0]?.host.entryCount).toBe(2);
    expect(replayEntries(frames).map((item) => item.id)).toEqual(["a", "b"]);
    expect(frames[1]?.host.final).toBe(true);
  });

  test("an incomplete replacement never displaces the previous snapshot", () => {
    const db = fixture();
    completeSnapshot(db, "snapshot-old", [entry("old", "provider/old")]);

    expect(ingest(db, welcome(2), "snapshot-new")).toBe("snapshot-started");
    expect(ingest(db, {
      t: "snapshot-chunk",
      entries: [entry("new-1", "provider/new")],
      final: true,
    }, "snapshot-new")).toBe("snapshot-incomplete");

    const frames = replay(db);
    expect(frames[0]?.snapshot_id).toBe("snapshot-old");
    expect(replayEntries(frames).map((item) => item.id)).toEqual(["old"]);

    expect(ingest(db, welcome(1), "snapshot-overflow")).toBe("snapshot-started");
    expect(ingest(db, {
      t: "snapshot-chunk",
      entries: [entry("extra-1", "provider/1"), entry("extra-2", "provider/2")],
      final: false,
    }, "snapshot-overflow")).toBe("snapshot-incomplete");
    expect(replayEntries(replay(db)).map((item) => item.id)).toEqual(["old"]);
  });
  test("defers live frames during staging and revisions them after the committed train", () => {
    const db = fixture();
    completeSnapshot(db, "snapshot-old", [entry("old", "provider/old")]);

    expect(ingest(db, welcome(1), "snapshot-new")).toBe("snapshot-started");
    const stagedState = ingestOutcome(db, { t: "state", state: state("mid-snapshot") });
    const stagedEntry = ingestOutcome(db, {
      t: "entry",
      entry: entry("current", "provider/live"),
    });
    expect(stagedState).toMatchObject({ result: "live-staged", coord_revision: 0, relays: [] });
    expect(stagedEntry).toMatchObject({ result: "live-staged", coord_revision: 0, relays: [] });

    const committed = ingestOutcome(db, {
      t: "snapshot-chunk",
      entries: [entry("current", "provider/snapshot")],
      final: true,
    }, "snapshot-new");
    expect(committed.result).toBe("snapshot-committed");
    expect(committed.relays.map((frame) => ({
      revision: frame.coord_revision,
      type: JSON.parse(frame.frame_json).t,
    }))).toEqual([
      { revision: 5, type: "state" },
      { revision: 6, type: "entry" },
    ]);

    const frames = replay(db);
    expect(frames.map((frame) => frame.coord_revision)).toEqual([3, 4, 5, 6]);
    expect(frames.map((frame) => frame.host.t)).toEqual([
      "welcome", "snapshot-chunk", "state", "entry",
    ]);
    expect(replayEntries(frames)).toEqual([entry("current", "provider/live")]);
  });


  test("live entries upsert by OMP source id and retain stable order", () => {
    const db = fixture();
    completeSnapshot(db, "snapshot-1", [
      entry("a", "provider/original"),
      entry("b", "provider/b"),
    ]);

    expect(ingest(db, { t: "entry", entry: entry("a", "provider/updated") })).toBe("live-persisted");
    expect(ingest(db, { t: "entry", entry: entry("c", "provider/c") })).toBe("live-persisted");
    expect(ingest(db, { t: "state", state: state("latest") })).toBe("live-persisted");
    expect(ingest(db, { t: "agents", agents: [{
      id: "sub-1",
      displayName: "worker",
      kind: "sub",
      status: "running",
      hasSessionFile: true,
      createdAt: NOW,
      lastActivity: NOW,
    }] })).toBe("live-persisted");

    const frames = replay(db);
    const entries = replayEntries(frames);
    expect(entries.map((item) => item.id)).toEqual(["a", "b", "c"]);
    expect(entries[0]?.model).toBe("provider/updated");
    expect(frames.at(-2)?.host).toMatchObject({ t: "state", state: { sessionName: "latest" } });
    expect(frames.at(-1)?.host).toMatchObject({ t: "agents", agents: [{ id: "sub-1" }] });
  });

  test("replay holds one SQLite revision while live updates continue", () => {
    const db = fixture(true);
    completeSnapshot(db, "snapshot-1", [entry("a", "provider/a")]);

    const stream = replayAgentUiSnapshot(db, SESSION_ID);
    const first = stream.next();
    expect(first.value && JSON.parse(first.value.frame_json)).toMatchObject({
      t: "welcome",
      entryCount: 1,
    });

    ingest(db, { t: "entry", entry: entry("b", "provider/b") });
    const rest = [...stream].map((frame) => JSON.parse(frame.frame_json));
    const streamedIds = rest.flatMap((frame) => frame.t === "snapshot-chunk"
      ? frame.entries.map((item: { id: string }) => item.id)
      : []);
    expect(streamedIds).toEqual(["a"]);
    expect(replayEntries(replay(db)).map((item) => item.id)).toEqual(["a", "b"]);
  });

  test("replay chunks stay bounded and stale staging is swept", () => {
    const db = fixture();
    const entries = Array.from({ length: 8 }, (_, index) =>
      entry(`entry-${index}`, `provider/${"x".repeat(90_000)}`));
    expect(ingest(db, welcome(entries.length), "snapshot-1")).toBe("snapshot-started");
    entries.forEach((item, index) => {
      const result = ingest(db, {
        t: "snapshot-chunk",
        entries: [item],
        final: index === entries.length - 1,
      }, "snapshot-1");
      expect(result).toBe(index === entries.length - 1
        ? "snapshot-committed"
        : "snapshot-staged");
    });

    const frames = [...replayAgentUiSnapshot(db, SESSION_ID)];
    expect(frames.every((frame) => Buffer.byteLength(frame.frame_json) <= AGENT_UI_FRAME_MAX_BYTES))
      .toBe(true);
    const chunks = frames.map((frame) => JSON.parse(frame.frame_json))
      .filter((frame) => frame.t === "snapshot-chunk");
    expect(chunks.length).toBeGreaterThan(1);
    expect(chunks.at(-1)?.final).toBe(true);
    expect(chunks.slice(0, -1).every((frame) => frame.final === false)).toBe(true);

    const staleAt = NOW - 2 * 60 * 60 * 1000;
    ingest(db, welcome(1), "stale-snapshot", staleAt);
    expect(ingestOutcome(
      db,
      { t: "entry", entry: entry("after-stale", "provider/live") },
      "",
      staleAt,
    ).result).toBe("live-staged");
    const swept = sweepAgentUiSnapshotStaging(db, NOW);
    expect(swept.deleted).toBe(1);
    expect(swept.relays.map((relay) => ({
      sessionId: relay.session_id,
      type: JSON.parse(relay.frame_json).t,
    }))).toEqual([{ sessionId: SESSION_ID, type: "entry" }]);
    expect(replayEntries(replay(db)).map((item) => item.id)).toContain("after-stale");
    expect(db.query<{ count: number }, []>(
      "SELECT COUNT(*) AS count FROM agent_ui_snapshot_staging",
    ).get()?.count).toBe(0);
  });
});
