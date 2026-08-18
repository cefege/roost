// Hop-deadline semantics for the typed terminal-control path.
//
// Three nested budgets bound one browser intent: the coordinator's result
// deadline, the worker's relative pre-write budget carved out of whatever is
// left of it, and the keeper's own reconciliation budget inside that. This
// file pins the coordinator half of that contract:
//
//   * an expiry BEFORE the worker send is a definite rejection — nothing was
//     written, provisional state is unwound, and a retry cannot duplicate;
//   * a worker rejection is honoured as definite only when its phase proves
//     the keeper never wrote;
//   * every other post-send outcome is ambiguous, never presented as unsent
//     and never auto-retried as a fresh write;
//   * wall-clock skew/steps on either host change nothing, because only
//     monotonic elapsed time and RELATIVE budgets are ever used;
//   * a viewport parked on its worker result releases the lane at send
//     admission, so the next PTY input is written exactly once, immediately.

import { create } from "@bufbuild/protobuf";
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  TerminalInputStatus,
  TerminalViewportStatus,
  TerminalWritePhase,
  WInputResultSchema,
  WViewportResultSchema,
  type CoordWorkerDown,
} from "@roost/shared/proto/worker_transport_pb";
import type { CoordConfig } from "@roost/shared/config";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { newJwtCache } from "../src/jwt.ts";
import { isSubscribed } from "../src/connect/cell-subscriptions.ts";
import { _setViewerTrackerDb, _viewersBySession } from "../src/connect/viewer-tracker.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import { resolvePendingRpc } from "../src/router/pending-rpcs.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import {
  processInputControl,
  processViewportControl,
  type TerminalViewerIdentity,
} from "../src/connect/session-control.ts";
import {
  startHopDeadline,
  INPUT_CONTROL_TIMEOUT_MS,
  VIEWPORT_CONTROL_TIMEOUT_MS,
  type HopDeadline,
} from "../src/connect/worker-send.ts";

const WORKER_FP = "abadcafe".repeat(8);
const CALLER_FP = "feedface".repeat(8);

let workdir: string;
let db: KyselyDB;
let deps: ConnectDeps;
let cleanup: () => void;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-hop-deadline-"));
  const keyPath = join(workdir, "test.key");
  const authPath = join(workdir, "authorized_keys");
  writeFileSync(authPath, "");
  const opened = openDb(join(workdir, "test.db"));
  db = opened.db;
  await runMigrations(opened.sqlite);
  _setViewerTrackerDb(db);
  const cfg: CoordConfig = {
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
  };
  await db.insertInto("workers").values({
    fp: WORKER_FP, label: "hop", os: "linux", reachable_addr: "127.0.0.1",
    git_sha: null, host_metrics_json: null,
    registered_at_ms: Date.now(), last_seen_ms: Date.now(),
  }).execute();

  cleanup = () => {
    __setConnectWorkerForTest(WORKER_FP, null);
    try { opened.sqlite.close(); } catch { /* ignore */ }
    if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
  };
});

afterAll(() => cleanup?.());

let sessionCounter = 0;
async function seedSession(): Promise<string> {
  sessionCounter += 1;
  const id = `hop00000-0000-4000-8000-${String(sessionCounter).padStart(12, "0")}`;
  await db.insertInto("sessions").values({
    id, worker_fp: WORKER_FP, channel: 1, kind: "shell",
    cwd: "/tmp", status: "open", created_at: Date.now(),
  }).execute();
  return id;
}

function identity(tabId: string): TerminalViewerIdentity {
  return { viewerKey: `${CALLER_FP}:${tabId}`, callerFingerprint: CALLER_FP };
}

/** A deadline frozen at `remainingMs`. Nothing here reads a clock, so a test
 * asserts the classification rule itself rather than racing a real timer. */
function frozenDeadline(totalMs: number, remainingMs: number): HopDeadline {
  return { totalMs, remainingMs: () => remainingMs };
}

function attachWorker(send: (frame: CoordWorkerDown) => number): void {
  __setConnectWorkerForTest(WORKER_FP, { workerFp: WORKER_FP, send });
}

describe("pre-send expiry is a definite rejection with no mutation", () => {
  test("an exhausted viewport budget never reaches the socket and unwinds membership", async () => {
    const sessionId = await seedSession();
    const viewer = identity("tab-viewport-presend");
    const frames: CoordWorkerDown[] = [];
    attachWorker((frame) => { frames.push(frame); return 1; });

    const result = await processViewportControl(deps, {
      identity: viewer, sessionId, clientSeq: 1n, cols: 80, rows: 24, cause: 1,
      deadline: frozenDeadline(VIEWPORT_CONTROL_TIMEOUT_MS, 0),
    });

    expect(result.status).toBe("rejected");
    if (result.status !== "rejected") throw new Error("expected rejection");
    expect(result.reason).toBe("viewport budget expired before worker send");
    expect(frames).toHaveLength(0);
    expect(isSubscribed(viewer.viewerKey, sessionId)).toBe(false);
    expect(_viewersBySession.get(sessionId)?.has(viewer.viewerKey) ?? false).toBe(false);
  });

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
  test("REJECTED + PRE_WRITE rolls provisional membership back", async () => {
    const sessionId = await seedSession();
    const viewer = identity("tab-prewrite-reject");
    attachWorker((frame) => {
      if (frame.frame.case === "viewportRequest") {
        const request = frame.frame.value;
        resolvePendingRpc(request.requestId, create(WViewportResultSchema, {
          requestId: request.requestId, sessionId: request.sessionId,
          clientSeq: request.clientSeq, status: TerminalViewportStatus.REJECTED,
          phase: TerminalWritePhase.PRE_WRITE,
          reason: "viewport budget expired before keeper write",
        }));
      }
      return 1;
    });

    const result = await processViewportControl(deps, {
      identity: viewer, sessionId, clientSeq: 1n, cols: 80, rows: 24, cause: 1,
    });

    expect(result.status).toBe("rejected");
    if (result.status !== "rejected") throw new Error("expected rejection");
    expect(result.reason).toBe("viewport budget expired before keeper write");
    expect(isSubscribed(viewer.viewerKey, sessionId)).toBe(false);
    expect(_viewersBySession.get(sessionId)?.has(viewer.viewerKey) ?? false).toBe(false);
  });

  test("REJECTED after the write is downgraded to ambiguous and keeps membership", async () => {
    const sessionId = await seedSession();
    const viewer = identity("tab-postwrite-reject");
    attachWorker((frame) => {
      if (frame.frame.case === "viewportRequest") {
        const request = frame.frame.value;
        // A worker that says "rejected" but cannot prove it stopped before the
        // resize may already have moved the PTY; unwinding here would let a
        // retry issue a second resize against a changed core.
        resolvePendingRpc(request.requestId, create(WViewportResultSchema, {
          requestId: request.requestId, sessionId: request.sessionId,
          clientSeq: request.clientSeq, status: TerminalViewportStatus.REJECTED,
          phase: TerminalWritePhase.UNKNOWN, reason: "keeper result lost",
        }));
      }
      return 1;
    });

    const result = await processViewportControl(deps, {
      identity: viewer, sessionId, clientSeq: 1n, cols: 90, rows: 30, cause: 1,
    });

    expect(result.status).toBe("ambiguous");
    expect(isSubscribed(viewer.viewerKey, sessionId)).toBe(true);
    expect(_viewersBySession.get(sessionId)?.get(viewer.viewerKey)).toMatchObject({
      cols: 90, rows: 30,
    });
  });

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

describe("post-send uncertainty is ambiguous, never unsent", () => {
  test("a lost worker result expires the correlation as ambiguous without a second write", async () => {
    const sessionId = await seedSession();
    const viewer = identity("tab-lost-result");
    let sends = 0;
    attachWorker((frame) => {
      if (frame.frame.case === "viewportRequest") sends += 1;
      return 1;
    });

    // 1.2s remaining leaves ~450ms of coordinator wait after the worker slice,
    // so this finishes as a real deadline expiry rather than a stubbed reply.
    const result = await processViewportControl(deps, {
      identity: viewer, sessionId, clientSeq: 1n, cols: 100, rows: 40, cause: 1,
      deadline: frozenDeadline(VIEWPORT_CONTROL_TIMEOUT_MS, 1_200),
    });

    expect(result.status).toBe("ambiguous");
    if (result.status !== "ambiguous") throw new Error("expected ambiguity");
    expect(result.reason).toMatch(/did not reply within \d+ms/);
    expect(sends).toBe(1);
    // Membership survives so a newer monotonic claim reconciles it; a rollback
    // here would be the fabricated rejection this path exists to prevent.
    expect(isSubscribed(viewer.viewerKey, sessionId)).toBe(true);
  });

  test("a dropped socket write is definite, not ambiguous", async () => {
    const sessionId = await seedSession();
    const viewer = identity("tab-dropped-write");
    attachWorker(() => 0);

    const result = await processViewportControl(deps, {
      identity: viewer, sessionId, clientSeq: 1n, cols: 80, rows: 24, cause: 1,
    });

    expect(result.status).toBe("rejected");
    if (result.status !== "rejected") throw new Error("expected rejection");
    expect(result.reason).toBe("worker unavailable");
    expect(isSubscribed(viewer.viewerKey, sessionId)).toBe(false);
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

describe("a blocked viewport cannot block or duplicate PTY input", () => {
  test("input is written exactly once while the viewport result is still pending", async () => {
    const sessionId = await seedSession();
    const viewer = identity("tab-hol");
    const order: string[] = [];
    let inputWrites = 0;
    const viewportSent = Promise.withResolvers<void>();

    attachWorker((frame) => {
      if (frame.frame.case === "viewportRequest") {
        // Deliberately never settled: the viewport is parked on a keeper
        // resize for its whole budget.
        order.push("viewport");
        viewportSent.resolve();
        return 1;
      }
      if (frame.frame.case === "inputRequest") {
        order.push("input");
        inputWrites += 1;
        const request = frame.frame.value;
        resolvePendingRpc(request.requestId, create(WInputResultSchema, {
          requestId: request.requestId, sessionId: request.sessionId,
          inputSeq: request.inputSeq, status: TerminalInputStatus.ACCEPTED,
          writtenBytes: request.data.byteLength,
          phase: TerminalWritePhase.WRITTEN,
        }));
      }
      return 1;
    });

    const viewportResult = processViewportControl(deps, {
      identity: viewer, sessionId, clientSeq: 1n, cols: 120, rows: 40, cause: 1,
      deadline: frozenDeadline(VIEWPORT_CONTROL_TIMEOUT_MS, 1_200),
    });
    await viewportSent.promise;

    // Same viewer + session, so this shares the lane the viewport is holding.
    const inputResult = await processInputControl(deps, {
      identity: viewer, sessionId, inputSeq: 1n, data: Uint8Array.of(0x0d),
    });

    expect(inputResult.status).toBe("accepted");
    expect(inputWrites).toBe(1);
    // Send order is still viewport-then-input: the lane splits at admission,
    // it does not disappear.
    expect(order).toEqual(["viewport", "input"]);

    expect((await viewportResult).status).toBe("ambiguous");
    // The parked viewport never provoked a second input write.
    expect(inputWrites).toBe(1);
  });

  test("an older in-flight rejection cannot unwind the successor it overlaps", async () => {
    const sessionId = await seedSession();
    const viewer = identity("tab-overlap");
    const parked: Array<{ requestId: string; sessionId: string; clientSeq: bigint }> = [];
    const firstSent = Promise.withResolvers<void>();

    attachWorker((frame) => {
      if (frame.frame.case !== "viewportRequest") return 1;
      const request = frame.frame.value;
      if (parked.length === 0) {
        parked.push(request);
        firstSent.resolve();
        return 1;
      }
      resolvePendingRpc(request.requestId, create(WViewportResultSchema, {
        requestId: request.requestId, sessionId: request.sessionId,
        clientSeq: request.clientSeq, status: TerminalViewportStatus.COMMITTED,
        channelResizeSeq: 9n, cols: request.cols, rows: request.rows,
        resized: true, phase: TerminalWritePhase.WRITTEN,
      }));
      return 1;
    });

    const stale = processViewportControl(deps, {
      identity: viewer, sessionId, clientSeq: 1n, cols: 80, rows: 24, cause: 1,
    });
    await firstSent.promise;

    // Only reachable because the lane released at admission: the successor
    // runs while its predecessor's result is still outstanding.
    const successor = await processViewportControl(deps, {
      identity: viewer, sessionId, clientSeq: 2n, cols: 132, rows: 43, cause: 2,
    });
    expect(successor.status).toBe("accepted");

    // The stale attempt now answers with a proved pre-write rejection. Its
    // rollback is identity-guarded, so the newer claim survives intact.
    const first = parked[0]!;
    resolvePendingRpc(first.requestId, create(WViewportResultSchema, {
      requestId: first.requestId, sessionId: first.sessionId,
      clientSeq: first.clientSeq, status: TerminalViewportStatus.REJECTED,
      phase: TerminalWritePhase.PRE_WRITE, reason: "superseded before keeper write",
    }));
    expect((await stale).status).toBe("rejected");
    expect(isSubscribed(viewer.viewerKey, sessionId)).toBe(true);
    expect(_viewersBySession.get(sessionId)?.get(viewer.viewerKey)).toMatchObject({
      cols: 132, rows: 43,
    });
  });
});
