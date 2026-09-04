// Owns isolated SQLite and route state for the durable-publication test modules.
// Each discovered suite invokes its reset and close methods from Bun lifecycle hooks.
// It depends on coord migrations, event appends, byte-hub state, and worker registry state.

import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb, type DbHandle } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { appendEvent, type AppendEventResult } from "../src/event-log.ts";
import {
  applyDurableChannelIndex,
  resetWorkerChannelIndexReconcile,
} from "../src/byte-hub.ts";
import { connectWorkers } from "../src/connect/worker-registry.ts";
import {
  SessionEvent,
  asChannelId,
  asSessionId,
  asWorkerFp,
  type Session,
} from "@roost/shared/wire";

interface DurablePublicationFixtureOptions {
  slug: string;
  primaryFingerprintByte: string;
  secondaryFingerprintByte: string;
  sessionGroup: string;
}

export function createDurablePublicationFixture(
  options: DurablePublicationFixtureOptions,
) {
  const FP = asWorkerFp(options.primaryFingerprintByte.repeat(32));
  const OTHER_FP = asWorkerFp(options.secondaryFingerprintByte.repeat(32));
  const SID_A = asSessionId(`00000000-0000-4000-8000-0000000000a${options.sessionGroup}`);
  const SID_B = asSessionId(`00000000-0000-4000-8000-0000000000b${options.sessionGroup}`);
  const SID_C = asSessionId(`00000000-0000-4000-8000-0000000000c${options.sessionGroup}`);
  const ORGANIZATION_ID = `durable-publication-${options.slug}-organization`;
  const DASHBOARD_ID = `durable-publication-${options.slug}-dashboard`;

  let workdir = "";
  let writer: DbHandle | undefined;
  // A separate WAL reader sees only committed transactions, making visibility an ordering proof.
  let reader: DbHandle | undefined;
  let clientSeq = 0;

  function openedEvent(sid: string, channel: number, workerFp = FP): SessionEvent {
    return SessionEvent.parse({
      kind: "opened", session_id: asSessionId(sid), worker_fp: workerFp,
      channel: asChannelId(channel), session_kind: "shell", cwd: "/tmp", ts: 1,
    });
  }

  function respawnedEvent(sid: string, channel: number): SessionEvent {
    return SessionEvent.parse({
      kind: "respawned", session_id: asSessionId(sid),
      new_channel: asChannelId(channel), ts: 2,
    });
  }

  function liveSession(sid: string, channel: number, workerFp = FP): Session {
    return {
      id: asSessionId(sid), worker_fp: workerFp, channel: asChannelId(channel),
      kind: "shell", cwd: "/tmp", workspace_id: null, status: "open",
      created_at: 1000, closed_at: null, custom_title: null,
    };
  }

  function snapshotEvent(sessions: Session[], workerFp = FP): SessionEvent {
    return { kind: "snapshot", worker_fp: workerFp, sessions, ts: 5000 };
  }

  // Distinct client_seq values keep ordinary appends out of the intentional replay-dedupe path.
  function append(
    event: SessionEvent,
    workerFp: string | null = FP,
  ): Promise<AppendEventResult> {
    if (!writer) throw new Error("durable publication fixture is not initialized");
    clientSeq += 1;
    return appendEvent(writer.db, event, {
      worker_fp: workerFp,
      client_seq: workerFp === null ? null : clientSeq,
      dashboardId: DASHBOARD_ID,
    });
  }

  function nextClientSeq(): number {
    clientSeq += 1;
    return clientSeq;
  }

  function committedChannel(sid: string): number | null {
    if (!reader) throw new Error("durable publication fixture is not initialized");
    const row = reader.sqlite.query("select channel from sessions where id = ?").get(sid);
    if (!row || typeof row !== "object" || !("channel" in row)) return null;
    return typeof row.channel === "number" ? row.channel : null;
  }

  async function reset(): Promise<void> {
    if (!workdir) {
      workdir = mkdtempSync(join(tmpdir(), `roost-durable-pub-${options.slug}-`));
      const dbPath = join(workdir, "coord.db");
      writer = openDb(dbPath);
      await runMigrations(writer.sqlite);
      await writer.db.insertInto("organizations").values({
        id: ORGANIZATION_ID,
        slug: `durable-publication-${options.slug}`,
        name: "Durable publication",
        status: "active",
        created_at_ms: 1,
      }).execute();
      await writer.db.insertInto("dashboards").values({
        id: DASHBOARD_ID,
        organization_id: ORGANIZATION_ID,
        slug: `durable-publication-${options.slug}`,
        name: "Durable publication",
        status: "active",
        created_at_ms: 1,
      }).execute();
      for (const workerFp of [FP, OTHER_FP]) {
        await writer.db.insertInto("workers").values({
          dashboard_id: DASHBOARD_ID,
          fp: workerFp, label: "test", os: "linux", git_sha: null, host_metrics_json: null,
          registered_at_ms: 1, last_seen_ms: 1,
        }).execute();
      }
      reader = openDb(dbPath);
    }
    connectWorkers.delete(FP);
    connectWorkers.delete(OTHER_FP);
    for (const sid of [SID_A, SID_B, SID_C]) {
      applyDurableChannelIndex(
        SessionEvent.parse({ kind: "closed", session_id: sid, exit_code: 0, ts: 9 }),
        null,
      );
    }
    resetWorkerChannelIndexReconcile(FP);
    resetWorkerChannelIndexReconcile(OTHER_FP);
    await writer!.db.deleteFrom("sessions").execute();
    await writer!.db.deleteFrom("events").execute();
  }

  async function close(): Promise<void> {
    connectWorkers.delete(FP);
    connectWorkers.delete(OTHER_FP);
    try {
      await reader?.close();
    } finally {
      try {
        await writer?.close();
      } finally {
        if (workdir && existsSync(workdir)) {
          rmSync(workdir, { recursive: true, force: true });
        }
      }
    }
  }

  return {
    FP,
    OTHER_FP,
    SID_A,
    SID_B,
    SID_C,
    DASHBOARD_ID,
    get writer(): DbHandle {
      if (!writer) throw new Error("durable publication fixture is not initialized");
      return writer;
    },
    get clientSeq(): number {
      return clientSeq;
    },
    openedEvent,
    respawnedEvent,
    liveSession,
    snapshotEvent,
    append,
    nextClientSeq,
    committedChannel,
    reset,
    close,
  };
}
