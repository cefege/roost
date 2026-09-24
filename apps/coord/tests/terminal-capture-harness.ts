// Shared fixtures for the terminal-capture suites: identifiers, the request and
// evidence builders, a fake worker connection that scripts capture
// acknowledgements over the real pending-RPC table, and the migrated
// single-tenant database the bridge resolves its session scope from.
// Used by terminal-capture-bridge.test.ts, terminal-capture-recorder.test.ts
// and diag-snapshot-handlers.test.ts.

import { create } from "@bufbuild/protobuf";
import { expect } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { CellGridFrame, CellRow, CellSpan } from "@roost/protocol/cell";
import {
  TerminalCaptureAction,
  TerminalCaptureRequestSchema,
  type TerminalCaptureRequest,
} from "@roost/protocol/proto/coordinator_pb";
import {
  type TerminalCaptureCommand,
  type TerminalCaptureResult,
} from "@roost/protocol/terminal-capture";
import type { AccountDeviceCaller } from "../src/connect/auth-principal.ts";
import { recordCoordinatorFrame } from "../src/connect/terminal-capture-recorder.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { resolvePendingRpc } from "../src/router/pending-rpcs.ts";
import { ensureSelfHostedTenant, type SelfHostedTenant } from "../src/self-hosted-tenant.ts";
import { EPOCH, SESSION as HUB_SESSION, STREAM } from "./terminal-screen-hub-harness.ts";

export const CAPTURE_WORKER = "b2c3d4e5".repeat(8);
/** The hub harness's session, so hub-driven records and bridge leases agree. */
export const SESSION_A = HUB_SESSION;
export const SESSION_B = "40000000-0000-4000-8000-0000000000b2";
export const SESSION_C = "40000000-0000-4000-8000-0000000000c3";
export const RECLAIMED_SESSION = "40000000-0000-4000-8000-0000000000d4";
export const UNKNOWN_SESSION = "40000000-0000-4000-8000-0000000000ee";
export const RECORDING_A = "70000000-0000-4000-8000-0000000000a1";
export const RECORDING_B = "70000000-0000-4000-8000-0000000000b1";
export const RECORDING_C = "70000000-0000-4000-8000-0000000000c1";
export const CAPTURE_1 = "80000000-0000-4000-8000-000000000001";
export const CAPTURE_2 = "80000000-0000-4000-8000-000000000002";
export const CAPTURE_3 = "80000000-0000-4000-8000-000000000003";
export const WORKER_CAPTURE_PATH =
  "/home/roost/.roost/logs/terminal-incident-80000000-0000-4000-8000-000000000001.json.gz";
/** A token no response field and no log line may ever repeat: it stands in for
 *  the terminal text a browser bundle legitimately carries to the worker. */
export const EVIDENCE_MARKER = "FOOTER-14s-secret";

export type CaptureReply =
  | { readonly kind: "ack"; readonly data: Record<string, unknown> }
  | { readonly kind: "park" }
  | { readonly kind: "drop" };

export interface ParkedCapture {
  readonly requestId: string;
  readonly action: string;
}

export interface CaptureWorkerLog {
  readonly commands: Record<string, unknown>[];
  readonly parked: ParkedCapture[];
  replies: CaptureReply[];
}

export interface CaptureFixture {
  readonly db: KyselyDB;
  readonly tenant: SelfHostedTenant;
  readonly deviceA: AccountDeviceCaller;
  readonly deviceB: AccountDeviceCaller;
  close(): Promise<void>;
}

export function createCaptureWorkerLog(): CaptureWorkerLog {
  return { commands: [], parked: [], replies: [] };
}

export function captureWorkerAck(action: string): Record<string, unknown> {
  const captured = action === "capture";
  return {
    status: action === "start" ? "recording" : action === "stop" ? "stopped" : "captured",
    path: captured ? WORKER_CAPTURE_PATH : null,
    byte_length: captured ? 4_096 : null,
    error: null,
    expires_at_ms: captured ? 1_800_000 : null,
    recent_worker_capture: null,
  };
}

/** Answers a `diag-terminal-capture` browser command with the next scripted
 *  reply. Returns null for any other frame so a caller with its own commands
 *  falls through to them. */
export function handleCaptureFrame(
  log: CaptureWorkerLog,
  workerFp: string,
  requestId: string,
  frameJson: string,
): number | null {
  const command = JSON.parse(frameJson) as Record<string, unknown>;
  if (command.kind !== "diag-terminal-capture") return null;
  log.commands.push(command);
  const action = String(command.action);
  const reply = log.replies.shift() ?? { kind: "ack", data: captureWorkerAck(action) };
  if (reply.kind === "drop") return 0;
  if (reply.kind === "park") {
    log.parked.push({ requestId, action });
    return 1;
  }
  expect(resolvePendingRpc(requestId, reply.data, workerFp)).toBe(true);
  return 1;
}

export function installCaptureWorker(log: CaptureWorkerLog, workerFp = CAPTURE_WORKER): void {
  __setConnectWorkerForTest(workerFp, {
    workerFp,
    send(frame) {
      if (frame.frame.case !== "browserCommand") throw new Error("unexpected capture frame");
      const sent = handleCaptureFrame(
        log,
        workerFp,
        frame.frame.value.requestId,
        frame.frame.value.frameJson,
      );
      if (sent === null) throw new Error("unexpected browser command on the capture worker");
      return sent;
    },
  });
}

export function captureRequestMessage(overrides: Partial<{
  action: TerminalCaptureAction;
  sessionId: string;
  recordingId: string;
  captureId: string;
  reason: string;
  browserEvidenceJson: string;
}> = {}): TerminalCaptureRequest {
  return create(TerminalCaptureRequestSchema, {
    action: overrides.action ?? TerminalCaptureAction.START,
    sessionId: overrides.sessionId ?? SESSION_A,
    recordingId: overrides.recordingId ?? RECORDING_A,
    captureId: overrides.captureId ?? CAPTURE_1,
    reason: overrides.reason ?? "manual",
    browserEvidenceJson: overrides.browserEvidenceJson ?? "",
  });
}

/** One already-validated capture command, as the bridge hands it to an
 *  envelope check or the worker call. */
export function captureCommand(
  overrides: Partial<TerminalCaptureCommand> = {},
): TerminalCaptureCommand {
  return {
    action: "capture",
    session_id: SESSION_A,
    recording_id: RECORDING_A,
    capture_id: CAPTURE_1,
    reason: "manual",
    browser_evidence_json: "",
    ...overrides,
  };
}

/** One accepted full at `seq`, as TerminalScreenHub's hook would report it. */
export function recordOneCoordinatorFrame(sessionId: string, seq: number): void {
  recordCoordinatorFrame(
    sessionId,
    canonicalCaptureFrame({ seq, cols: 8, rows: 1 }),
    { full: true, seq: BigInt(seq), baseSeq: 0n },
    new Map(),
  );
}

export function captureResultOf(response: { snapshotJson?: string }): {
  payload: Record<string, unknown>;
  capture: TerminalCaptureResult;
} {
  if (response.snapshotJson === undefined) {
    throw new Error("DiagSnapshot response omitted snapshot JSON");
  }
  const payload = JSON.parse(response.snapshotJson) as Record<string, unknown>;
  return { payload, capture: payload.terminal_capture as TerminalCaptureResult };
}

/** One migrated database with a registered worker, three open sessions and a
 *  fourth the reclaim test closes. */
export async function openCaptureFixture(): Promise<CaptureFixture> {
  const workdir = mkdtempSync(join(tmpdir(), "roost-terminal-capture-"));
  const opened = openDb(join(workdir, "coord.db"));
  await runMigrations(opened.sqlite);
  const tenant = ensureSelfHostedTenant(opened.sqlite, { backfillLegacyScopes: false });
  await opened.db.insertInto("workers").values([{
    fp: CAPTURE_WORKER,
    dashboard_id: tenant.dashboardId,
    label: "capture",
    os: "linux",
    registered_at_ms: 1,
    last_seen_ms: 1,
  }]).execute();
  await opened.db.insertInto("sessions").values(
    [SESSION_A, SESSION_B, SESSION_C, RECLAIMED_SESSION].map((id, index) => ({
      id,
      dashboard_id: tenant.dashboardId,
      worker_fp: CAPTURE_WORKER,
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
    deviceA: {
      kind: "account-device",
      fingerprint: "device-a",
      label: "device a",
      accountId: tenant.accountId,
    },
    deviceB: {
      kind: "account-device",
      fingerprint: "device-b",
      label: "device b",
      accountId: tenant.accountId,
    },
    async close() {
      await opened.close();
      rmSync(workdir, { recursive: true, force: true });
    },
  };
}

function span(text: string): CellSpan {
  return { text, columns: text.length, fg: 256, bg: 256, flags: 0 };
}

/** A canonical viewport shaped like the hub's own post-admission frame. */
export function canonicalCaptureFrame(options: {
  seq: number;
  rows?: number;
  text?: string;
  spansPerRow?: number;
  epoch?: string;
  cols?: number;
}): CellGridFrame {
  const rows = options.rows ?? 2;
  const spansPerRow = options.spansPerRow ?? 1;
  const viewportRows: CellRow[] = Array.from({ length: rows }, (_, index) => ({
    index,
    spans: Array.from({ length: spansPerRow }, () => span(options.text ?? `row-${index}`)),
  }));
  return {
    streamId: STREAM,
    gridEpoch: options.epoch ?? EPOCH,
    cols: options.cols ?? 80,
    rows,
    cursorRow: 0,
    cursorCol: 0,
    cursorVisible: true,
    altScreen: false,
    cursorKeysApp: false,
    bracketedPaste: false,
    mouseTracking: 0,
    mouseSgr: false,
    focusEvents: false,
    full: true,
    viewportRows,
    scrollbackRows: [],
    scrollbackAppend: [],
    scrollbackTotal: 0,
    sbBase: 0,
    baseSeq: 0,
    seq: options.seq,
  };
}
