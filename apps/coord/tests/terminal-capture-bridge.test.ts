// Terminal capture bridge: lease ownership, server-time expiry, capture
// admission (one outstanding, manual cooldown, bounded idempotency) and the
// worker-failure mapping that must come back as a result the browser can retry.
// Fixtures — database, fake worker, request builders — live in
// terminal-capture-harness.ts.

import { Code } from "@connectrpc/connect";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { TerminalCaptureAction } from "@roost/protocol/proto/coordinator_pb";
import {
  TERMINAL_CAPTURE_LIMITS,
  TERMINAL_INCIDENT_SCHEMA,
  checkTerminalCaptureEnvelope,
  type TerminalCaptureCoordinatorPayload,
  type TerminalCaptureResult,
} from "@roost/protocol/terminal-capture";
import type { AccountDeviceCaller } from "../src/connect/auth-principal.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import {
  createTerminalCaptureBridge,
  type TerminalCaptureBridge,
} from "../src/connect/terminal-capture.ts";
import {
  _resetTerminalCaptureLeases,
  _sweepTerminalCaptureRecordings,
} from "../src/connect/terminal-capture-lease.ts";
import {
  coordinatorRecorderArmed,
  _coordinatorRecorderStats,
  _resetCoordinatorRecorder,
} from "../src/connect/terminal-capture-recorder.ts";
import { requestTerminalCapture } from "../src/connect/terminal-capture-worker-call.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import { rejectPendingRpcsForWorker, resolvePendingRpc } from "../src/router/pending-rpcs.ts";
import {
  CAPTURE_1,
  CAPTURE_2,
  CAPTURE_3,
  CAPTURE_WORKER,
  RECLAIMED_SESSION,
  RECORDING_A,
  RECORDING_B,
  RECORDING_C,
  SESSION_A,
  SESSION_B,
  SESSION_C,
  UNKNOWN_SESSION,
  WORKER_CAPTURE_PATH,
  captureCommand,
  captureRequestMessage,
  captureWorkerAck,
  createCaptureWorkerLog,
  installCaptureWorker,
  openCaptureFixture,
  recordOneCoordinatorFrame,
  type CaptureFixture,
  type CaptureWorkerLog,
} from "./terminal-capture-harness.ts";
import {
  browserEvidenceJson,
  browserEvidencePayload,
  withoutLayerSection,
} from "./terminal-capture-evidence.ts";

let fixture: CaptureFixture;
let bridge: TerminalCaptureBridge;
let worker: CaptureWorkerLog;

function actions(): string[] {
  return worker.commands.map((command) => String(command.action));
}

async function start(
  sessionId = SESSION_A,
  recordingId = RECORDING_A,
  principal: AccountDeviceCaller = fixture.deviceA,
): Promise<TerminalCaptureResult> {
  return await bridge.handle(
    captureRequestMessage({ action: TerminalCaptureAction.START, sessionId, recordingId }),
    principal,
  );
}

async function capture(overrides: Partial<{
  sessionId: string;
  recordingId: string;
  captureId: string;
  browserEvidenceJson: string;
}> = {}, principal: AccountDeviceCaller = fixture.deviceA): Promise<TerminalCaptureResult> {
  return await bridge.handle(
    captureRequestMessage({ action: TerminalCaptureAction.CAPTURE, ...overrides }),
    principal,
  );
}

async function stop(
  recordingId = RECORDING_A,
  principal: AccountDeviceCaller = fixture.deviceA,
): Promise<TerminalCaptureResult> {
  return await bridge.handle(
    captureRequestMessage({ action: TerminalCaptureAction.STOP, recordingId }),
    principal,
  );
}

beforeAll(async () => {
  fixture = await openCaptureFixture();
  bridge = createTerminalCaptureBridge({ db: fixture.db } as unknown as ConnectDeps);
});

beforeEach(() => {
  worker = createCaptureWorkerLog();
  installCaptureWorker(worker);
});

afterEach(() => {
  rejectPendingRpcsForWorker(CAPTURE_WORKER, "test cleanup");
  __setConnectWorkerForTest(CAPTURE_WORKER, null);
  _resetTerminalCaptureLeases();
  _resetCoordinatorRecorder();
});

afterAll(async () => {
  await fixture?.close();
});

describe("terminal capture lease", () => {
  test("arms the coordinator recorder and renews without clearing evidence", async () => {
    const armed = await start();
    expect(armed).toMatchObject({
      session_id: SESSION_A,
      recording_id: RECORDING_A,
      action: "start",
      status: "recording",
      worker_fp: CAPTURE_WORKER,
      path: null,
      error: null,
    });
    expect(armed.expires_at_ms).toBeGreaterThan(Date.now());
    expect(coordinatorRecorderArmed(SESSION_A)).toBe(true);
    recordOneCoordinatorFrame(SESSION_A, 1);

    const renewed = await start();
    expect(renewed.status).toBe("recording");
    expect(renewed.expires_at_ms!).toBeGreaterThanOrEqual(armed.expires_at_ms!);
    expect(_coordinatorRecorderStats(SESSION_A))
      .toMatchObject({ recordingId: RECORDING_A, records: 1 });
    expect(actions()).toEqual(["start", "start"]);
  });

  test("another recording or another device cannot take an armed session", async () => {
    await start();
    await expect(start(SESSION_A, RECORDING_B)).rejects.toMatchObject({
      code: Code.AlreadyExists,
      rawMessage: "lease_conflict: recording_id",
    });
    await expect(start(SESSION_A, RECORDING_B, fixture.deviceB))
      .rejects.toMatchObject({ code: Code.AlreadyExists });
    await expect(start(SESSION_A, RECORDING_A, fixture.deviceB)).rejects.toMatchObject({
      code: Code.PermissionDenied,
      rawMessage: "permission_denied: recording_id",
    });
    // The refused calls never reached the worker and never disturbed the lease.
    expect(actions()).toEqual(["start"]);
    expect(_coordinatorRecorderStats(SESSION_A)).toMatchObject({ recordingId: RECORDING_A });
  });

  test("an expired lease disarms the recorder and refuses a late capture", async () => {
    await start();
    _sweepTerminalCaptureRecordings(Date.now() + TERMINAL_CAPTURE_LIMITS.leaseMs + 1);
    expect(coordinatorRecorderArmed(SESSION_A)).toBe(false);

    await expect(capture({ browserEvidenceJson: browserEvidenceJson() })).rejects.toMatchObject({
      code: Code.FailedPrecondition,
      rawMessage: "lease_expired: recording_id",
    });
    expect(actions()).toEqual(["start"]);

    // Re-arming is an explicit START, never a silent renewal.
    expect((await start()).status).toBe("recording");
    expect(coordinatorRecorderArmed(SESSION_A)).toBe(true);
  });

  test("stop is owner-only, frees coordinator state and stays idempotent", async () => {
    await start();
    await expect(stop(RECORDING_A, fixture.deviceB)).rejects.toMatchObject({
      code: Code.PermissionDenied,
      rawMessage: "permission_denied: recording_id",
    });
    expect(coordinatorRecorderArmed(SESSION_A)).toBe(true);

    const stopped = await stop();
    expect(stopped).toMatchObject({ action: "stop", status: "stopped", expires_at_ms: null });
    expect(coordinatorRecorderArmed(SESSION_A)).toBe(false);
    expect((await stop()).status).toBe("stopped");
    expect(actions()).toEqual(["start", "stop", "stop"]);
  });

  test("a third recording is refused without evicting either live one", async () => {
    await start(SESSION_A, RECORDING_A);
    await start(SESSION_B, RECORDING_B);
    await expect(start(SESSION_C, RECORDING_C)).rejects.toMatchObject({
      code: Code.ResourceExhausted,
      rawMessage: "resource_exhausted: recording_id",
    });
    expect(coordinatorRecorderArmed(SESSION_A)).toBe(true);
    expect(coordinatorRecorderArmed(SESSION_B)).toBe(true);
  });

  test("a lease on a closed session is reclaimed instead of parking a slot", async () => {
    await start(SESSION_A, RECORDING_A);
    await start(RECLAIMED_SESSION, RECORDING_B);
    await fixture.db.updateTable("sessions").set({ status: "closed" })
      .where("id", "=", RECLAIMED_SESSION).execute();

    expect((await start(SESSION_C, RECORDING_C)).status).toBe("recording");
    expect(coordinatorRecorderArmed(SESSION_A)).toBe(true);
    expect(coordinatorRecorderArmed(RECLAIMED_SESSION)).toBe(false);
    await fixture.db.updateTable("sessions").set({ status: "open" })
      .where("id", "=", RECLAIMED_SESSION).execute();
  });

  test("an unreachable session never reaches a lease or the worker", async () => {
    await expect(start(UNKNOWN_SESSION, RECORDING_A)).rejects.toMatchObject({
      code: Code.NotFound,
      rawMessage: "session_unknown: session_id",
    });
    expect(worker.commands).toEqual([]);
    expect(coordinatorRecorderArmed(UNKNOWN_SESSION)).toBe(false);
  });

  test("a worker that refuses START rolls the fresh arm back", async () => {
    worker.replies = [{ kind: "drop" }];
    expect(await start()).toMatchObject({
      status: "error",
      error: "worker_offline",
      expires_at_ms: null,
    });
    expect(coordinatorRecorderArmed(SESSION_A)).toBe(false);
  });
});

describe("terminal capture admission", () => {
  test("carries frozen coordinator evidence and answers the worker's file", async () => {
    await start();
    recordOneCoordinatorFrame(SESSION_A, 3);

    expect(await capture({ browserEvidenceJson: browserEvidenceJson() })).toMatchObject({
      action: "capture",
      status: "captured",
      path: WORKER_CAPTURE_PATH,
      byte_length: 4_096,
      expires_at_ms: 1_800_000,
      error: null,
    });
    const frame = worker.commands[worker.commands.length - 1]!;
    expect(frame.kind).toBe("diag-terminal-capture");
    expect(frame.recording_id).toBe(RECORDING_A);
    // Each layer crosses the wire as an envelope with its section NESTED under
    // the layer's own member; the worker unwraps that member.
    const forwarded = JSON.parse(
      String(frame.coordinator_evidence_json),
    ) as TerminalCaptureCoordinatorPayload;
    expect(forwarded).toMatchObject({
      schema: TERMINAL_INCIDENT_SCHEMA,
      layer: "coordinator",
      capture_id: CAPTURE_1,
      recording_id: RECORDING_A,
      session_id: SESSION_A,
    });
    expect(forwarded.coordinator.records).toHaveLength(1);
    expect(forwarded.coordinator.captured_at_ms).toBeGreaterThan(0);
    expect(checkTerminalCaptureEnvelope(
      String(frame.coordinator_evidence_json),
      { layer: "coordinator", command: captureCommand() },
    ).ok).toBe(true);
    // The browser's own payload reached the worker unchanged, and freezing did
    // not drain the ring the lease keeps recording into.
    expect(JSON.parse(String(frame.browser_evidence_json)))
      .toMatchObject({ layer: "browser", browser: { layer: "browser" } });
    expect(_coordinatorRecorderStats(SESSION_A))
      .toMatchObject({ recordingId: RECORDING_A, records: 1 });
  });

  test("browser evidence with no nested section never reaches the worker", async () => {
    await start();
    await expect(capture({
      browserEvidenceJson: JSON.stringify(withoutLayerSection(browserEvidencePayload())),
    })).rejects.toMatchObject({
      code: Code.InvalidArgument,
      rawMessage: "evidence_malformed: browser",
    });
    expect(actions()).toEqual(["start"]);
  });

  test("an idempotent retry returns the original result and dispatches once", async () => {
    await start();
    const first = await capture();
    expect(await capture()).toEqual(first);
    expect(actions()).toEqual(["start", "capture"]);
  });

  test("one capture at a time per session, then a manual cooldown", async () => {
    await start();
    worker.replies = [{ kind: "park" }];
    const inFlight = capture();
    await expect(capture({ captureId: CAPTURE_2 })).rejects.toMatchObject({
      code: Code.Aborted,
      rawMessage: "capture_in_flight: capture_id",
    });

    const held = worker.parked[0]!;
    expect(resolvePendingRpc(held.requestId, captureWorkerAck("capture"), CAPTURE_WORKER)).toBe(true);
    expect((await inFlight).status).toBe("captured");

    await expect(capture({ captureId: CAPTURE_3 })).rejects.toMatchObject({
      code: Code.ResourceExhausted,
      rawMessage: "rate_limited: capture_id",
    });
  });

  test("an unarmed one-shot captures but never overrides an active lease", async () => {
    const oneShot = await capture({
      sessionId: SESSION_B,
      recordingId: RECORDING_B,
      browserEvidenceJson: browserEvidenceJson({
        recordingId: RECORDING_B,
        sessionId: SESSION_B,
      }),
    });
    expect(oneShot).toMatchObject({ status: "captured", path: WORKER_CAPTURE_PATH });
    // No lease was allocated, nothing was armed, and the coordinator reports
    // its own layer absent rather than inventing rows.
    expect(coordinatorRecorderArmed(SESSION_B)).toBe(false);
    expect(String(worker.commands[0]!.coordinator_evidence_json)).toBe("");

    await start(SESSION_A, RECORDING_A, fixture.deviceB);
    await expect(capture({ recordingId: RECORDING_C, captureId: CAPTURE_2 })).rejects.toMatchObject({
      code: Code.AlreadyExists,
      rawMessage: "lease_conflict: recording_id",
    });
    expect(_coordinatorRecorderStats(SESSION_A)).toMatchObject({ recordingId: RECORDING_A });
  });

  test("a worker-reported failure is a result, not a throw", async () => {
    await start();
    worker.replies = [{
      kind: "ack",
      data: {
        status: "error",
        path: null,
        byte_length: null,
        error: "storage_failed",
        expires_at_ms: null,
        recent_worker_capture: {
          capture_id: CAPTURE_3,
          path: WORKER_CAPTURE_PATH,
          byte_length: 128,
          status: "captured",
        },
      },
    }];
    expect(await capture()).toMatchObject({
      status: "error",
      error: "storage_failed",
      path: null,
      recent_worker_capture: { capture_id: CAPTURE_3, byte_length: 128 },
    });
    // The failure is not cached and the lease still records, so the browser can
    // retry the same capture ID with its frozen evidence.
    expect(coordinatorRecorderArmed(SESSION_A)).toBe(true);
  });

  test("an unparseable worker acknowledgement is a worker failure", async () => {
    await start();
    worker.replies = [{
      kind: "ack",
      data: { status: "captured", path: WORKER_CAPTURE_PATH, byte_length: -1, error: null },
    }];
    expect(await capture()).toMatchObject({
      status: "error",
      error: "worker_failed",
      path: null,
    });
  });

  test("a deadline and a dropped socket map to their own fixed codes", async () => {
    worker.replies = [{ kind: "park" }];
    // The contract's deadline is ten seconds; the mapping is what this proves.
    expect(await requestTerminalCapture(CAPTURE_WORKER, captureCommand(), "", 25))
      .toEqual({ ok: false, code: "worker_timeout" });
    __setConnectWorkerForTest(CAPTURE_WORKER, null);
    expect(await requestTerminalCapture(CAPTURE_WORKER, captureCommand(), ""))
      .toEqual({ ok: false, code: "worker_offline" });
  });
});
