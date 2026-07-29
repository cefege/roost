// omp agent sessions, coord side. Pins the three things the worker slice and
// the SPA compile against:
//   1. spawn: SessionsSpawn{kind:"agent"} → the `spawn-agent` control frame.
//   2. transcript backfill: SessionsGetAgentEntries → the worker's
//      `get-omp-transcript-page` frame, and its {entries, first_seq, more}
//      rpc-ok payload decoded back into proto AgentEntries. A malformed entry
//      is skipped, not fatal; a lost worker is Code.Unavailable, NEVER an
//      empty page (an empty page reads as "no history" and would leave a
//      permanent hole in the rendered transcript).
//   3. fan-out: an AgentEntriesFrame on globalAgentEntryBus becomes
//      FirehoseFrame.agentEntries — the SPA's live path. Mirrors
//      cell-passthrough.test.ts for the cell bus.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Database } from "bun:sqlite";
import type { HandlerContext, ServiceImpl } from "@connectrpc/connect";
import { ConnectError, Code } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  CoordinatorService,
  SessionsGetAgentEntriesRequestSchema, SessionsAgentRespondRequestSchema,
  SessionsAgentAbortRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import { AgentEntriesFrameSchema, type FirehoseFrame } from "@roost/shared/proto/sync_pb";
import type { CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";
import { asSessionId, asWorkerFp } from "@roost/shared/wire";
import type { ClientControlFrame } from "@roost/shared/wire";
import { agentEntryToProto } from "@roost/shared/wire/agent-proto";
import { AgentEntry } from "@roost/shared/wire/agent-entry";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { globalAgentEntryBus } from "../src/buses.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-service.ts";
import { _spawnFrameFor } from "../src/connect/handlers-sessions.ts";
import { makeAgentSessionHandlers } from "../src/connect/handlers-sessions-agent.ts";
import { startSyncFeed } from "../src/connect/handlers-streaming.ts";
import { resolvePendingRpc, rejectPendingRpcsForWorker } from "../src/router/pending-rpcs.ts";
import type { ConnectDeps } from "../src/connect/router.ts";

type AgentHandlers = Pick<
  ServiceImpl<typeof CoordinatorService>,
  "sessionsGetAgentEntries" | "sessionsAgentRespond" | "sessionsAgentAbort"
>;

const FP = asWorkerFp("ae".repeat(32));
const SID = asSessionId("11111111-1111-4111-8111-111111111111");
const MISSING_SID = asSessionId("99999999-9999-4999-8999-999999999999");

let workdir: string;
let db: KyselyDB;
let sqlite: Database;
let handlers: AgentHandlers;

// Downstream browser-commands, FIFO. `waiter` lets a test await the SEND itself
// (the handlers read the DB first, so the frame lands a hop after the call);
// a wall-clock sleep would be slower and would race under load.
const sent: CoordWorkerDown[] = [];
let waiter: (() => void) | null = null;

const connect = (): void => {
  __setConnectWorkerForTest(String(FP), {
    workerFp: String(FP),
    send: (f) => { sent.push(f); const w = waiter; waiter = null; w?.(); return 1; },
  });
};

/** Await + decode the next downstream browser-command's control frame. */
async function nextFrame(): Promise<{ requestId: string; frame: ClientControlFrame }> {
  if (sent.length === 0) {
    const { promise, resolve } = Promise.withResolvers<void>();
    waiter = resolve;
    await promise;
  }
  const f = sent.shift();
  if (f?.frame.case !== "browserCommand") throw new Error("downstream frame is not a browser-command");
  return {
    requestId: f.frame.value.requestId,
    frame: JSON.parse(f.frame.value.frameJson) as ClientControlFrame,
  };
}

function entry(seq: number, text: string): unknown {
  return { kind: "assistant", seq, ts: 1000 + seq, text, done: true };
}

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-agent-entries-"));
  const opened = openDb(join(workdir, "test.db"));
  db = opened.db; sqlite = opened.sqlite;
  await runMigrations(sqlite);
  await db.insertInto("workers").values({
    fp: String(FP), label: "test", os: "darwin", git_sha: null, host_metrics_json: null,
    registered_at_ms: 1, last_seen_ms: 1,
  }).execute();
  await db.insertInto("sessions").values({
    id: SID, worker_fp: String(FP), channel: 3, kind: "agent", cwd: "/tmp/scratch",
    workspace_id: null, status: "open", agent_json: null, created_at: 1, closed_at: null,
  }).execute();
  handlers = makeAgentSessionHandlers({ db } as unknown as ConnectDeps);
  connect();
});

afterAll(() => {
  __setConnectWorkerForTest(String(FP), null);
  try { sqlite.close(); } catch { /* ignore */ }
  if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
});

describe("agent session spawn frame", () => {
  it("kind 'agent' spawns an omp child and carries no PTY geometry", () => {
    expect(_spawnFrameFor({ kind: "agent", folder: "/tmp/scratch", sessionId: SID }))
      .toEqual({ kind: "spawn-agent", folder: "/tmp/scratch", session_id: SID });
    // cols/rows offered by a terminal-shaped caller are dropped: an omp child
    // has no grid to size.
    expect(_spawnFrameFor({ kind: "agent", folder: "/tmp/scratch", cols: 80, rows: 24 }))
      .toEqual({ kind: "spawn-agent", folder: "/tmp/scratch" });
  });
});

describe("SessionsGetAgentEntries", () => {
  it("asks the worker for a transcript page and decodes {entries, first_seq, more}", async () => {
    const res = Promise.resolve(handlers.sessionsGetAgentEntries(
      create(SessionsGetAgentEntriesRequestSchema, { sessionId: SID, beforeSeq: 0n }), ctx(),
    ));

    const { requestId, frame } = await nextFrame();
    if (frame.kind !== "get-omp-transcript-page") throw new Error(`wrong frame: ${frame.kind}`);
    expect(frame.session_id).toBe(SID);
    expect(frame.limit).toBe(128);
    expect(frame.cursor).toBe("0");           // 0 = newest page
    expect(frame.request_id).toBe(requestId); // frame id == envelope id

    // The worker's rpc-ok payload: Zod-shaped AgentEntry JSON. The middle entry
    // is malformed (a tool entry with no body fields) and must be dropped
    // without costing the rest of the page.
    expect(resolvePendingRpc(requestId, {
      entries: [entry(4, "pong"), { kind: "tool", seq: 5, ts: 1 }, entry(6, "done")],
      first_seq: 4,
      more: true,
    })).toBe(true);

    const out = await res;
    const entries = out.entries ?? [];
    expect(entries.map((e) => Number(e.seq))).toEqual([4, 6]);
    expect(entries[0]?.body?.case).toBe("assistant");
    expect(out.firstSeq).toBe(4n);
    expect(out.more).toBe(true);
  });

  it("non-zero before_seq rides through as the cursor", async () => {
    const res = Promise.resolve(handlers.sessionsGetAgentEntries(
      create(SessionsGetAgentEntriesRequestSchema, { sessionId: SID, beforeSeq: 900n }), ctx(),
    ));
    const { requestId, frame } = await nextFrame();
    if (frame.kind !== "get-omp-transcript-page") throw new Error(`wrong frame: ${frame.kind}`);
    expect(frame.cursor).toBe("900");
    resolvePendingRpc(requestId, { entries: [], first_seq: 0, more: false });
    const out = await res;
    expect(out.entries).toEqual([]);
    expect(out.more).toBe(false);
  });

  it("a lost worker is Unavailable, never an empty page", async () => {
    const res = Promise.resolve(handlers.sessionsGetAgentEntries(
      create(SessionsGetAgentEntriesRequestSchema, { sessionId: SID, beforeSeq: 0n }), ctx(),
    ));
    await nextFrame(); // the pending rpc exists once the frame is out
    expect(rejectPendingRpcsForWorker(String(FP), "worker disconnected")).toBe(1);
    const err = await res.then(() => null, (e: unknown) => e);
    expect(err).toBeInstanceOf(ConnectError);
    expect((err as ConnectError).code).toBe(Code.Unavailable);
  });

  it("unknown session is NotFound", async () => {
    const err = await Promise.resolve(handlers.sessionsGetAgentEntries(
      create(SessionsGetAgentEntriesRequestSchema, { sessionId: MISSING_SID, beforeSeq: 0n }), ctx(),
    )).then(() => null, (e: unknown) => e);
    expect((err as ConnectError).code).toBe(Code.NotFound);
  });
});

describe("prompt answer + abort forwarding", () => {
  it("agent-respond carries the prompt id verbatim", async () => {
    const out = await handlers.sessionsAgentRespond(create(SessionsAgentRespondRequestSchema, {
      sessionId: SID, promptId: "ui-req-7", value: "Approve", cancelled: false,
    }), ctx());
    expect(out.accepted).toBe(true);
    const { requestId, frame } = await nextFrame();
    expect(frame).toEqual({
      kind: "agent-respond", request_id: requestId, session_id: SID,
      prompt_id: "ui-req-7", value: "Approve", cancelled: false,
    });
  });

  it("cancelled answers forward too (a wedged prompt hangs the agent forever)", async () => {
    await handlers.sessionsAgentRespond(create(SessionsAgentRespondRequestSchema, {
      sessionId: SID, promptId: "ui-req-8", value: "", cancelled: true,
    }), ctx());
    const { frame } = await nextFrame();
    if (frame.kind !== "agent-respond") throw new Error(`wrong frame: ${frame.kind}`);
    expect(frame.cancelled).toBe(true);
  });

  it("abort forwards omp-abort", async () => {
    const out = await handlers.sessionsAgentAbort(
      create(SessionsAgentAbortRequestSchema, { sessionId: SID }), ctx(),
    );
    expect(out.accepted).toBe(true);
    const { requestId, frame } = await nextFrame();
    expect(frame).toEqual({ kind: "omp-abort", request_id: requestId, session_id: SID });
  });

  it("offline worker reports accepted:false instead of throwing", async () => {
    __setConnectWorkerForTest(String(FP), null);
    const out = await handlers.sessionsAgentAbort(
      create(SessionsAgentAbortRequestSchema, { sessionId: SID }), ctx(),
    );
    expect(out.accepted).toBe(false);
    expect(sent.length).toBe(0);
    connect();
  });
});

describe("globalAgentEntryBus → FirehoseFrame.agentEntries", () => {
  it("fans a worker batch out to the SPA untouched, and stops on dispose", () => {
    const pushed: FirehoseFrame[] = [];
    const feed = startSyncFeed({ db } as unknown as ConnectDeps, 0, (f) => {
      if (f.frame.case === "agentEntries") pushed.push(f);
    });
    const pb = agentEntryToProto(AgentEntry.parse(entry(9, "hello")));
    globalAgentEntryBus.publish(create(AgentEntriesFrameSchema, { sessionId: SID, entries: [pb] }));

    expect(pushed.length).toBe(1);
    const f = pushed[0]!;
    if (f.frame.case !== "agentEntries") throw new Error("wrong frame");
    expect(f.frame.value.sessionId).toBe(SID);
    expect(f.frame.value.entries.map((e) => Number(e.seq))).toEqual([9]);

    feed.dispose();
    globalAgentEntryBus.publish(create(AgentEntriesFrameSchema, { sessionId: SID }));
    expect(pushed.length).toBe(1);
  });
});

// callerKey is module-private, so a real ContextValues can't be pre-authed from
// out here; requireAuth only ever reads the caller, hence the flat stub.
function ctx(): HandlerContext {
  return {
    values: { get: () => ({ fingerprint: "b".repeat(64), label: "browser" }), set: () => {} },
  } as unknown as HandlerContext;
}
