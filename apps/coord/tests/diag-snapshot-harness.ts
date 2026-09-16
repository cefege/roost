// Fixtures for the DiagSnapshot handler suite: the migrated single-tenant
// database, the authenticated/anonymous handler contexts, the request builder,
// and one fake worker connection that answers diagnostic snapshots, terminal
// pipeline snapshots and terminal-capture commands over the real pending-RPC
// table. Capture-specific builders live in terminal-capture-harness.ts.
// Used by diag-snapshot-handlers.test.ts.

import { create } from "@bufbuild/protobuf";
import { createContextValues, type HandlerContext } from "@connectrpc/connect";
import { expect } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  DiagSnapshotRequestSchema,
  type DiagSnapshotRequest,
  type TerminalCaptureRequest,
} from "@roost/shared/proto/coordinator_pb";
import { WTerminalPipelineSnapshotSchema } from "@roost/shared/proto/worker_transport_pb";
import { TerminalPipelineSessionSnapshotSchema } from "@roost/shared/proto/wire_pb";
import { callerKey } from "../src/connect/auth-interceptor.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { resolvePendingRpc } from "../src/router/pending-rpcs.ts";
import { ensureSelfHostedTenant, type SelfHostedTenant } from "../src/self-hosted-tenant.ts";
import {
  createCaptureWorkerLog,
  handleCaptureFrame,
  type CaptureWorkerLog,
} from "./terminal-capture-harness.ts";

export const WORKER_A = "a1b2c3d4".repeat(8);
export const WORKER_C = "c3d4e5f6".repeat(8);
export const WORKER_LOCAL = "d4e5f6a7".repeat(8);
export const LOCAL_UNSELECTED_SESSION = "91000000-0000-4000-8000-000000000065";
export const MISSING_SESSION = "91000000-0000-4000-8000-000000000066";
export const BATCH_SESSION_IDS = Array.from(
  { length: 64 },
  (_, index) => `91000000-0000-4000-8000-${String(index + 1).padStart(12, "0")}`,
);
export const DIAG_WORKER_FPS = [WORKER_A, WORKER_C, WORKER_LOCAL];

export interface SnapshotPayload {
  coord: { sessions: Record<string, unknown> };
  workers: Record<string, {
    status: string;
    snapshot?: { sessions: Record<string, unknown> };
    terminal_pipeline?: {
      status: string;
      snapshot?: { sessions: Array<{ session_id: string }> };
    };
  }>;
  truncated?: true;
}

export interface DiagWorkerLog {
  readonly sentWorkerFps: string[];
  readonly pipelineTargetsByWorker: Record<string, Array<{
    sessionId: string;
    viewId: string;
  }>>;
  readonly capture: CaptureWorkerLog;
}

export interface DiagSnapshotFixture {
  readonly db: KyselyDB;
  readonly tenant: SelfHostedTenant;
  close(): Promise<void>;
}

const diagnosticSessionsByWorker: Record<string, Record<string, unknown>> = {
  [WORKER_A]: Object.fromEntries(BATCH_SESSION_IDS.slice(0, 32).map((id) => [id, {}])),
  [WORKER_C]: Object.fromEntries(BATCH_SESSION_IDS.slice(32).map((id) => [id, {}])),
  [WORKER_LOCAL]: { [LOCAL_UNSELECTED_SESSION]: {} },
};

export function createDiagWorkerLog(): DiagWorkerLog {
  return {
    sentWorkerFps: [],
    pipelineTargetsByWorker: {},
    capture: createCaptureWorkerLog(),
  };
}

export function diagDeviceContext(tenant: SelfHostedTenant): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint: "diag-snapshot-device",
    label: "diagnostic test device",
    accountId: tenant.accountId,
  });
  return { values, signal: new AbortController().signal } as unknown as HandlerContext;
}

export function anonymousContext(): HandlerContext {
  return {
    values: createContextValues(),
    signal: new AbortController().signal,
  } as unknown as HandlerContext;
}

export function diagRequest(overrides: Partial<{
  sessionFilterId: string;
  sessionFilterIds: string[];
  terminalCapture: TerminalCaptureRequest;
}> = {}): DiagSnapshotRequest {
  return create(DiagSnapshotRequestSchema, overrides);
}

export function snapshotPayload(response: { snapshotJson?: string }): SnapshotPayload {
  if (response.snapshotJson === undefined) {
    throw new Error("DiagSnapshot response omitted snapshot JSON");
  }
  return JSON.parse(response.snapshotJson) as SnapshotPayload;
}

export function returnedSessionIds(snapshot: SnapshotPayload): string[] {
  return Object.values(snapshot.workers).flatMap((worker) =>
    worker.status === "ok" && worker.snapshot
      ? Object.keys(worker.snapshot.sessions)
      : []
  ).sort();
}

export function returnedPipelineSessionIds(snapshot: SnapshotPayload): string[] {
  return Object.values(snapshot.workers).flatMap((worker) =>
    worker.terminal_pipeline?.status === "ok" && worker.terminal_pipeline.snapshot
      ? worker.terminal_pipeline.snapshot.sessions.map((session) => session.session_id)
      : []
  ).sort();
}

export function installDiagWorker(log: DiagWorkerLog, workerFp: string): void {
  __setConnectWorkerForTest(workerFp, {
    workerFp,
    send(frame) {
      if (frame.frame.case === "browserCommand") {
        const request = frame.frame.value;
        const captured = handleCaptureFrame(
          log.capture,
          workerFp,
          request.requestId,
          request.frameJson,
        );
        if (captured !== null) return captured;
        expect(JSON.parse(request.frameJson)).toEqual({
          kind: "diag-snapshot",
          request_id: request.requestId,
        });
        log.sentWorkerFps.push(workerFp);
        expect(resolvePendingRpc(request.requestId, {
          captured_at_ms: 1,
          build: { git_sha: "test" },
          worker_fp: workerFp,
          sessions: diagnosticSessionsByWorker[workerFp] ?? {},
        }, workerFp)).toBe(true);
        return 1;
      }
      if (frame.frame.case === "terminalPipelineSnapshot") {
        const request = frame.frame.value;
        const targets = request.targets.map((target) => ({
          sessionId: target.sessionId,
          viewId: target.viewId,
        }));
        log.pipelineTargetsByWorker[workerFp] = targets;
        expect(resolvePendingRpc(request.requestId, create(WTerminalPipelineSnapshotSchema, {
          requestId: request.requestId,
          sessions: targets.map((target) =>
            create(TerminalPipelineSessionSnapshotSchema, target)),
        }), workerFp)).toBe(true);
        return 1;
      }
      throw new Error("unexpected diagnostic worker frame");
    },
  });
}

/** 64 batch sessions across two workers plus one unselected local session. */
export async function openDiagSnapshotFixture(): Promise<DiagSnapshotFixture> {
  const workdir = mkdtempSync(join(tmpdir(), "roost-diag-snapshot-handlers-"));
  const opened = openDb(join(workdir, "coord.db"));
  await runMigrations(opened.sqlite);
  const tenant = ensureSelfHostedTenant(opened.sqlite, { backfillLegacyScopes: false });
  await opened.db.insertInto("workers").values(
    [[WORKER_A, "A"], [WORKER_C, "C"], [WORKER_LOCAL, "local"]].map(([fp, label]) => ({
      fp: fp!,
      dashboard_id: tenant.dashboardId,
      label: label!,
      os: "linux",
      registered_at_ms: 1,
      last_seen_ms: 1,
    })),
  ).execute();
  await opened.db.insertInto("sessions").values(
    [...BATCH_SESSION_IDS, LOCAL_UNSELECTED_SESSION].map((id, index) => ({
      id,
      dashboard_id: tenant.dashboardId,
      worker_fp: index < 32 ? WORKER_A : index < 64 ? WORKER_C : WORKER_LOCAL,
      channel: index + 1,
      kind: "shell",
      cwd: "/tmp",
      status: "open",
      created_at: 1,
    })),
  ).execute();
  return {
    db: opened.db,
    tenant,
    async close() {
      await opened.close();
      rmSync(workdir, { recursive: true, force: true });
    },
  };
}
