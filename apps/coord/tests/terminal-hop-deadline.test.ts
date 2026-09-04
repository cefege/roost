// Hop-deadline semantics for the typed terminal-input path.
//
// Three nested budgets bound one browser intent: the coordinator's result
// deadline, the worker's relative pre-write budget carved out of whatever is
// left of it, and the keeper's own reconciliation budget inside that. This
// file pins the coordinator half of that contract:
//
//   * an expiry BEFORE the worker send is a definite rejection — nothing was
//     written, and a retry cannot duplicate;
//   * a worker rejection is honoured as definite only when its phase proves
//     the keeper never wrote;
//   * every other post-send outcome is ambiguous, never presented as unsent
//     and never auto-retried as a fresh write;
//   * wall-clock skew/steps on either host change nothing, because only
//     monotonic elapsed time and RELATIVE budgets are ever used.

import { create } from "@bufbuild/protobuf";
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  TerminalInputStatus,
  TerminalWritePhase,
  WInputResultSchema,
  type CoordWorkerDown,
} from "@roost/shared/proto/worker_transport_pb";
import type { CoordConfig } from "@roost/shared/config";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { newJwtCache } from "../src/jwt.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import { resolvePendingRpc } from "../src/router/pending-rpcs.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { PasswordWorkGate } from "../src/connect/password-work-gate.ts";
import { processInputControl } from "../src/connect/input-control.ts";
import type { TerminalViewerIdentity } from "../src/connect/terminal-control-lane.ts";
import {
  startHopDeadline,
  INPUT_CONTROL_TIMEOUT_MS,
  type HopDeadline,
} from "../src/connect/worker-send.ts";

const WORKER_FP = "abadcafe".repeat(8);
const CALLER_FP = "feedface".repeat(8);
const DASHBOARD_ID = "terminal-hop-deadline-dashboard";
const ORGANIZATION_ID = "terminal-hop-deadline-organization";

let workdir: string;
let db: KyselyDB;
let deps: ConnectDeps;
let cleanup: () => Promise<void>;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-hop-deadline-"));
  const keyPath = join(workdir, "test.key");
  const authPath = join(workdir, "authorized_keys");
  writeFileSync(authPath, "");
  const opened = openDb(join(workdir, "test.db"));
  db = opened.db;
  await runMigrations(opened.sqlite);
  const cfg: CoordConfig = {
    saasMode: false,
    managedContainer: false,
    pushAllowedOrigins: [],
    trustProxy: false, bind: "127.0.0.1:0", dbPath: join(workdir, "test.db"),
    coordKeyPath: keyPath, authorizedKeysPath: authPath, webDistPath: "",
    tlsCertPath: undefined, tlsKeyPath: undefined, jwtMaxAgeSecs: 300,
    auditRetentionDays: 90, relaxedCsp: false, corsAllowedOrigins: [],
    logDir: workdir, publicUrl: undefined,
    handoffPath: join(workdir, "coord-handoff.json"),
  };
  deps = {
    db,
    sqlite: opened.sqlite,
    coordKey: await loadOrCreateCoordKey(keyPath),
    cfg,
    jwtCache: newJwtCache(),
    passwordWorkGate: new PasswordWorkGate(),
  };
  const now = Date.now();
  await db.insertInto("organizations").values({
    id: ORGANIZATION_ID,
    slug: "terminal-hop-deadline",
    name: "Terminal hop deadline",
    status: "active",
    created_at_ms: now,
  }).execute();
  await db.insertInto("dashboards").values({
    id: DASHBOARD_ID,
    organization_id: ORGANIZATION_ID,
    slug: "terminal-hop-deadline",
    name: "Terminal hop deadline",
    status: "active",
    created_at_ms: now,
  }).execute();
  await db.insertInto("workers").values({
    dashboard_id: DASHBOARD_ID,
    fp: WORKER_FP, label: "hop", os: "linux", reachable_addr: "127.0.0.1",
    git_sha: null, host_metrics_json: null,
    registered_at_ms: now, last_seen_ms: now,
  }).execute();

  cleanup = async () => {
    __setConnectWorkerForTest(WORKER_FP, null);
    try { await opened.close(); } finally { if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true }); }
  };
});

afterAll(async () => { await cleanup?.(); });

let sessionCounter = 0;
async function seedSession(): Promise<string> {
  sessionCounter += 1;
  const id = `hop00000-0000-4000-8000-${String(sessionCounter).padStart(12, "0")}`;
  await db.insertInto("sessions").values({
    id, worker_fp: WORKER_FP, channel: 1, kind: "shell",
    dashboard_id: DASHBOARD_ID,
    cwd: "/tmp", status: "open", created_at: Date.now(),
  }).execute();
  return id;
}

function identity(tabId: string): TerminalViewerIdentity {
  return { viewerKey: `${CALLER_FP}:${tabId}`, callerFingerprint: CALLER_FP, dashboardId: DASHBOARD_ID };
}

/** A deadline frozen at `remainingMs`. Nothing here reads a clock, so a test
 * asserts the classification rule itself rather than racing a real timer. */
function frozenDeadline(totalMs: number, remainingMs: number): HopDeadline {
  return { totalMs, remainingMs: () => remainingMs };
}

function attachWorker(send: (frame: CoordWorkerDown) => number): void {
  __setConnectWorkerForTest(WORKER_FP, { workerFp: WORKER_FP, dashboardId: DASHBOARD_ID, send });
}

describe("pre-send expiry is a definite rejection", () => {
  test("a budget too short to survive the hop is refused rather than half-spent", async () => {
    const sessionId = await seedSession();
    const frames: CoordWorkerDown[] = [];
    attachWorker((frame) => { frames.push(frame); return 1; });

    // 800ms left cannot cover the return-trip reserve plus a usable worker
    // slice, so the send is skipped entirely instead of arriving with a budget
    // the worker could never answer inside.
    const result = await processInputControl(deps, {
      identity: identity("tab-input-presend"), sessionId, inputSeq: 1n,
      data: Uint8Array.of(0x61),
      deadline: frozenDeadline(INPUT_CONTROL_TIMEOUT_MS, 800),
    });

    expect(result.status).toBe("rejected");
    if (result.status !== "rejected") throw new Error("expected rejection");
    expect(result.reason).toBe("input budget expired before worker send");
    expect(result.writtenBytes).toBe(0);
    expect(frames).toHaveLength(0);
  });

  test("a healthy remaining budget still sends, and the worker slice is strictly smaller", async () => {
    const sessionId = await seedSession();
    const frames: CoordWorkerDown[] = [];
    attachWorker((frame) => {
      frames.push(frame);
      if (frame.frame.case === "inputRequest") {
        const request = frame.frame.value;
        resolvePendingRpc(request.requestId, create(WInputResultSchema, {
          requestId: request.requestId, sessionId: request.sessionId,
          inputSeq: request.inputSeq, status: TerminalInputStatus.ACCEPTED,
          writtenBytes: 1, phase: TerminalWritePhase.WRITTEN,
        }));
      }
      return 1;
    });

    const result = await processInputControl(deps, {
      identity: identity("tab-input-nested"), sessionId, inputSeq: 1n,
      data: Uint8Array.of(0x61),
      deadline: frozenDeadline(INPUT_CONTROL_TIMEOUT_MS, 4_000),
    });

    expect(result.status).toBe("accepted");
    expect(frames).toHaveLength(1);
    const sent = frames[0]!;
    if (sent.frame.case !== "inputRequest") throw new Error("expected input request");
    // Nesting is what guarantees the worker can answer while we still wait.
    expect(sent.frame.value.budgetMs).toBeGreaterThan(0);
    expect(sent.frame.value.budgetMs).toBeLessThan(4_000);
  });
});

describe("only a phase-proved rejection is definite", () => {
  test("input REJECTED without a pre-write phase is ambiguous, so it is never retried", async () => {
    const sessionId = await seedSession();
    let attempts = 0;
    attachWorker((frame) => {
      if (frame.frame.case === "inputRequest") {
        attempts += 1;
        const request = frame.frame.value;
        resolvePendingRpc(request.requestId, create(WInputResultSchema, {
          requestId: request.requestId, sessionId: request.sessionId,
          inputSeq: request.inputSeq, status: TerminalInputStatus.REJECTED,
          writtenBytes: 0, phase: TerminalWritePhase.WRITTEN,
          reason: "keeper write outcome unproven",
        }));
      }
      return 1;
    });

    const result = await processInputControl(deps, {
      identity: identity("tab-input-unproven"), sessionId, inputSeq: 1n,
      data: Uint8Array.of(0x6c, 0x73),
    });

    expect(result.status).toBe("ambiguous");
    expect(attempts).toBe(1);
  });
});

describe("wall-clock skew cannot change expiry semantics", () => {
  test("a wall clock stepped hours in either direction leaves the verdict intact", async () => {
    const sessionId = await seedSession();
    const frames: CoordWorkerDown[] = [];
    attachWorker((frame) => {
      frames.push(frame);
      if (frame.frame.case === "inputRequest") {
        const request = frame.frame.value;
        resolvePendingRpc(request.requestId, create(WInputResultSchema, {
          requestId: request.requestId, sessionId: request.sessionId,
          inputSeq: request.inputSeq, status: TerminalInputStatus.ACCEPTED,
          writtenBytes: 2, phase: TerminalWritePhase.WRITTEN,
        }));
      }
      return 1;
    });

    // The budget is monotonic, so a Date.now() that jumps a day forward or
    // back mid-flight is simply not consulted: same budget, same outcome.
    const deadline = startHopDeadline(INPUT_CONTROL_TIMEOUT_MS);
    const realNow = Date.now;
    const skews = [86_400_000, -86_400_000];
    try {
      let skewIndex = 0;
      Date.now = () => realNow() + skews[skewIndex % skews.length]!;
      const first = await processInputControl(deps, {
        identity: identity("tab-skew"), sessionId, inputSeq: 1n,
        data: Uint8Array.of(0x61, 0x62), deadline,
      });
      skewIndex += 1;
      const second = await processInputControl(deps, {
        identity: identity("tab-skew"), sessionId, inputSeq: 2n,
        data: Uint8Array.of(0x61, 0x62),
        deadline: startHopDeadline(INPUT_CONTROL_TIMEOUT_MS),
      });
      expect(first.status).toBe("accepted");
      expect(second.status).toBe("accepted");
    } finally {
      Date.now = realNow;
    }

    expect(frames).toHaveLength(2);
    for (const frame of frames) {
      if (frame.frame.case !== "inputRequest") throw new Error("expected input request");
      // A relative budget, never an absolute instant, is what makes this true.
      expect(frame.frame.value.budgetMs).toBeGreaterThan(0);
      expect(frame.frame.value.budgetMs).toBeLessThan(INPUT_CONTROL_TIMEOUT_MS);
    }
  });

  test("an expired budget stays expired no matter what the wall clock says", async () => {
    const sessionId = await seedSession();
    const frames: CoordWorkerDown[] = [];
    attachWorker((frame) => { frames.push(frame); return 1; });

    const realNow = Date.now;
    Date.now = () => realNow() - 3_600_000;
    try {
      const result = await processInputControl(deps, {
        identity: identity("tab-skew-expired"), sessionId, inputSeq: 1n,
        data: Uint8Array.of(0x61),
        deadline: frozenDeadline(INPUT_CONTROL_TIMEOUT_MS, -5),
      });
      expect(result.status).toBe("rejected");
      if (result.status !== "rejected") throw new Error("expected rejection");
      expect(result.reason).toBe("input budget expired before worker send");
    } finally {
      Date.now = realNow;
    }
    expect(frames).toHaveLength(0);
  });
});
