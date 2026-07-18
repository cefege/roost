// R5.1 invariant: every Zod schema round-trips. Catches schema-drift
// silently breaking the SPA after a worker/coord ships a new variant.

import { describe, test, expect } from "bun:test";
import {
  Worker, Session, SessionEvent, ClientControlFrame,
  Workspace, Task, WebhookToken, PermissionRule, McpRelay,
  WorkerPresenceEvent, WorkspaceDelta, TaskDelta, WebhookTokenDelta,
  PermissionRuleDelta, McpRelayEvent,
  CoordWorkerUpstream, CoordWorkerDownstream,
  foldAll, foldEvent,
} from "../src/wire/index.ts";

const FIXTURE_WORKER: Worker = {
  fp: "a".repeat(64) as Worker["fp"],
  label: "m1",
  os: "darwin",
  git_sha: "abc123def456",
  host_metrics: null,
  registered_at_ms: 1717000000000,
  last_seen_ms: 1717000000000,
  reachable_addr: null,
  keeper_stale: null,
};

const FIXTURE_SESSION: Session = {
  id: "00000000-0000-0000-0000-000000000001" as Session["id"],
  worker_fp: FIXTURE_WORKER.fp,
  channel: 1 as Session["channel"],
  kind: "shell",
  cwd: "/Users/you",
  workspace_id: null,
  status: "open",
  agent: null,
  created_at: 1717000000000,
  closed_at: null,
  custom_title: null,
};

function rt<T>(schema: { parse: (x: unknown) => T }, value: T): void {
  const json = JSON.stringify(value);
  const back = schema.parse(JSON.parse(json));
  expect(back).toEqual(value);
}

describe("R5.1 schema round-trip", () => {
  test("Worker", () => rt(Worker, FIXTURE_WORKER));
  test("Session", () => rt(Session, FIXTURE_SESSION));

  test("SessionEvent opened", () => {
    rt(SessionEvent, {
      kind: "opened",
      session_id: FIXTURE_SESSION.id,
      worker_fp: FIXTURE_WORKER.fp,
      channel: 1 as Session["channel"],
      session_kind: "shell",
      cwd: "/Users/you",
      ts: 1717000000000,
    });
  });

  test("ClientControlFrame attach", () => {
    rt(ClientControlFrame, {
      kind: "attach",
      session_id: FIXTURE_SESSION.id,
    });
  });

  // phase-24 — bidir coord↔worker WS wire schema. Sub-commits 24a-2 onward
  // consume these; the property tests guard the contract.
  test("CoordWorkerUpstream hello", () => {
    rt(CoordWorkerUpstream, {
      kind: "hello",
      worker_fp: FIXTURE_WORKER.fp,
      version: "v2",
    });
  });
  test("CoordWorkerUpstream event(opened)", () => {
    rt(CoordWorkerUpstream, {
      kind: "event",
      event: {
        kind: "opened",
        session_id: FIXTURE_SESSION.id,
        worker_fp: FIXTURE_WORKER.fp,
        channel: 1 as Session["channel"],
        session_kind: "shell",
        cwd: "/",
        ts: 1717000000001,
      },
    });
  });
  test("CoordWorkerUpstream rpc-ok", () => {
    rt(CoordWorkerUpstream, {
      kind: "rpc-ok",
      request_id: "req-0001",
      data: { ok: true, session_id: FIXTURE_SESSION.id },
    });
  });
  test("CoordWorkerDownstream hello-ack", () => {
    rt(CoordWorkerDownstream, {
      kind: "hello-ack",
      coord_pubkey_b64: "AAAA",
      coord_pubkey_kid: "0".repeat(64),
    });
  });
  test("CoordWorkerDownstream browser-command(attach)", () => {
    rt(CoordWorkerDownstream, {
      kind: "browser-command",
      browser_id: "browser-xyz",
      viewer_id: "viewer-xyz",
      request_id: "req-0002",
      frame: { kind: "attach", session_id: FIXTURE_SESSION.id },
    });
  });
});

describe("R5.2 event-log fold determinism", () => {
  test("fold(opened, closed) deletes the session (no closed limbo)", () => {
    const map = foldAll([
      { kind: "opened", session_id: FIXTURE_SESSION.id, worker_fp: FIXTURE_WORKER.fp, channel: 1 as Session["channel"], session_kind: "shell", cwd: "/", ts: 1 },
      { kind: "closed", session_id: FIXTURE_SESSION.id, exit_code: null, ts: 2 },
    ]);
    // A closed terminal is removed from the projection, not parked as a
    // status="closed" row. closed is the only deletion trigger.
    expect(map.size).toBe(0);
    expect(map.get(FIXTURE_SESSION.id)).toBeUndefined();
  });

  test("fold is deterministic across replays", () => {
    const events: Parameters<typeof foldEvent>[1][] = [
      { kind: "opened", session_id: FIXTURE_SESSION.id, worker_fp: FIXTURE_WORKER.fp, channel: 1 as Session["channel"], session_kind: "shell", cwd: "/", ts: 1 },
      { kind: "cwd", session_id: FIXTURE_SESSION.id, cwd: "/tmp", ts: 2 },
      { kind: "cwd", session_id: FIXTURE_SESSION.id, cwd: "/var", ts: 3 },
    ];
    const a = JSON.stringify(Array.from(foldAll(events).values()));
    const b = JSON.stringify(Array.from(foldAll(events).values()));
    expect(a).toBe(b);
    expect(JSON.parse(a)[0].cwd).toBe("/var");
  });

  test("snapshot KEEPS ghost sessions as breadcrumbs", () => {
    // Start with two open sessions for worker A.
    const sa1 = "00000000-0000-0000-0000-0000000000a1" as Session["id"];
    const sa2 = "00000000-0000-0000-0000-0000000000a2" as Session["id"];
    const sb = "00000000-0000-0000-0000-0000000000b1" as Session["id"];
    const workerA = "a".repeat(64) as Worker["fp"];
    const workerB = "b".repeat(64) as Worker["fp"];

    const map = foldAll([
      { kind: "opened", session_id: sa1, worker_fp: workerA, channel: 1 as Session["channel"], session_kind: "shell", cwd: "/", ts: 1 },
      { kind: "opened", session_id: sa2, worker_fp: workerA, channel: 2 as Session["channel"], session_kind: "shell", cwd: "/", ts: 2 },
      { kind: "opened", session_id: sb,  worker_fp: workerB, channel: 1 as Session["channel"], session_kind: "shell", cwd: "/", ts: 3 },
      // Worker A reconnects with only sa1 — sa2 is absent but PERSISTS as an
      // offline breadcrumb (only an explicit `closed`/✕ removes a row now).
      { kind: "snapshot", worker_fp: workerA, sessions: [{ ...FIXTURE_SESSION, id: sa1, worker_fp: workerA }], ts: 4 },
    ]);
    expect(map.has(sa1)).toBe(true);
    expect(map.has(sa2)).toBe(true);
    expect(map.has(sb)).toBe(true);
  });
});
