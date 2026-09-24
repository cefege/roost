// DiagSnapshot handler coverage: normalized session filters, worker fan-out,
// and the terminal-capture dispatch that must own its own response projection
// and never echo terminal content.
// Fixtures live in diag-snapshot-harness.ts and terminal-capture-harness.ts.

import { Code } from "@connectrpc/connect";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { setSignalSink } from "@roost/observability/diag";
import { TerminalCaptureAction } from "@roost/protocol/proto/coordinator_pb";
import {
  TERMINAL_CAPTURE_LIMITS,
  TERMINAL_INCIDENT_SCHEMA,
} from "@roost/protocol/terminal-capture";
import { makeSystemHandlers } from "../../src/rpc/handlers-system.ts";
import { __setConnectWorkerForTest } from "../../src/workers/worker-registry.ts";
import type { ConnectDeps } from "../../src/rpc/router.ts";
import { rejectPendingRpcsForWorker } from "../../src/router/pending-rpcs.ts";
import { _resetTerminalCaptureLeases } from "../../src/terminal/capture/terminal-capture-lease.ts";
import { _resetCoordinatorRecorder } from "../../src/terminal/capture/terminal-capture-recorder.ts";
import {
  BATCH_SESSION_IDS,
  DIAG_WORKER_FPS,
  LOCAL_UNSELECTED_SESSION,
  MISSING_SESSION,
  WORKER_A,
  WORKER_C,
  WORKER_LOCAL,
  anonymousContext,
  createDiagWorkerLog,
  diagDeviceContext,
  diagRequest,
  installDiagWorker,
  openDiagSnapshotFixture,
  returnedPipelineSessionIds,
  returnedSessionIds,
  snapshotPayload,
  type DiagSnapshotFixture,
  type DiagWorkerLog,
} from "./diag-snapshot-harness.ts";
import {
  browserEvidencePayload,
  withoutLayerSection,
} from "../terminal/capture/terminal-capture-evidence.ts";
import {
  EVIDENCE_MARKER,
  captureRequestMessage,
  captureResultOf,
} from "../terminal/capture/terminal-capture-harness.ts";

const CAPTURE_SESSION = BATCH_SESSION_IDS[0]!;
const CAPTURE_RECORDING = "92000000-0000-4000-8000-000000000001";
const CAPTURE_ID = "93000000-0000-4000-8000-000000000001";

let fixture: DiagSnapshotFixture;
let worker: DiagWorkerLog;
let emittedSignals: Record<string, unknown>[] = [];

function systemHandlers() {
  return makeSystemHandlers({ db: fixture.db } as unknown as ConnectDeps);
}

function deviceContext() {
  return diagDeviceContext(fixture.tenant);
}

function captureMessage(overrides: Partial<{
  action: TerminalCaptureAction;
  sessionId: string;
  browserEvidenceJson: string;
}> = {}) {
  return captureRequestMessage({
    sessionId: CAPTURE_SESSION,
    recordingId: CAPTURE_RECORDING,
    captureId: CAPTURE_ID,
    ...overrides,
  });
}

function capturePayload(sessionId = CAPTURE_SESSION) {
  return browserEvidencePayload({
    captureId: CAPTURE_ID,
    recordingId: CAPTURE_RECORDING,
    sessionId,
  });
}

function captureEvidence(sessionId = CAPTURE_SESSION): string {
  return JSON.stringify(capturePayload(sessionId));
}

beforeAll(async () => {
  fixture = await openDiagSnapshotFixture();
});

beforeEach(() => {
  worker = createDiagWorkerLog();
  emittedSignals = [];
  setSignalSink((record) => emittedSignals.push(record));
  for (const workerFp of DIAG_WORKER_FPS) installDiagWorker(worker, workerFp);
});

afterEach(() => {
  for (const workerFp of DIAG_WORKER_FPS) {
    rejectPendingRpcsForWorker(workerFp, "test cleanup");
    __setConnectWorkerForTest(workerFp, null);
  }
  setSignalSink(null);
  _resetTerminalCaptureLeases();
  _resetCoordinatorRecorder();
});

afterAll(async () => {
  await fixture?.close();
});

describe("DiagSnapshot session filters", () => {
  test("keeps singular session_filter_id compatibility", async () => {
    const sessionId = BATCH_SESSION_IDS[0]!;
    const snapshot = snapshotPayload(await systemHandlers().diagSnapshot(
      diagRequest({ sessionFilterId: sessionId }),
      deviceContext(),
    ));

    expect(Object.keys(snapshot.coord.sessions)).toEqual([sessionId]);
    expect(Object.keys(snapshot.workers)).toEqual([WORKER_A]);
    expect(returnedSessionIds(snapshot)).toEqual([sessionId]);
    expect(worker.sentWorkerFps).toEqual([WORKER_A]);
    expect(returnedPipelineSessionIds(snapshot)).toEqual([sessionId]);
    expect(worker.pipelineTargetsByWorker).toEqual({
      [WORKER_A]: [{ sessionId, viewId: "" }],
    });
  });

  test("admits 64 local IDs and targets only their workers", async () => {
    const snapshot = snapshotPayload(await systemHandlers().diagSnapshot(
      diagRequest({ sessionFilterIds: BATCH_SESSION_IDS }),
      deviceContext(),
    ));

    expect(Object.keys(snapshot.coord.sessions).sort()).toEqual([...BATCH_SESSION_IDS].sort());
    expect(Object.keys(snapshot.workers).sort()).toEqual([WORKER_A, WORKER_C]);
    expect(returnedSessionIds(snapshot)).toEqual([...BATCH_SESSION_IDS].sort());
    expect(worker.sentWorkerFps.sort()).toEqual([WORKER_A, WORKER_C]);
    expect(returnedPipelineSessionIds(snapshot)).toEqual([...BATCH_SESSION_IDS].sort());
    expect(Object.keys(worker.pipelineTargetsByWorker).sort()).toEqual([WORKER_A, WORKER_C]);
  });

  test("excludes unknown batch IDs and unrelated workers", async () => {
    const localSessionId = BATCH_SESSION_IDS[0]!;
    const snapshot = snapshotPayload(await systemHandlers().diagSnapshot(
      diagRequest({ sessionFilterIds: [localSessionId, MISSING_SESSION] }),
      deviceContext(),
    ));

    expect(Object.keys(snapshot.coord.sessions)).toEqual([localSessionId]);
    expect(Object.keys(snapshot.workers)).toEqual([WORKER_A]);
    expect(returnedSessionIds(snapshot)).toEqual([localSessionId]);
    expect(worker.sentWorkerFps).toEqual([WORKER_A]);
    expect(returnedPipelineSessionIds(snapshot)).toEqual([localSessionId]);
    expect(worker.pipelineTargetsByWorker).toEqual({
      [WORKER_A]: [{ sessionId: localSessionId, viewId: "" }],
    });
    expect(snapshot.workers[WORKER_LOCAL]).toBeUndefined();
  });

  test("rejects oversized and ambiguous filter input before dispatch", async () => {
    const handlers = systemHandlers();
    const sessionId = BATCH_SESSION_IDS[0]!;
    for (const request of [
      diagRequest({ sessionFilterIds: [...BATCH_SESSION_IDS, LOCAL_UNSELECTED_SESSION] }),
      diagRequest({ sessionFilterId: sessionId, sessionFilterIds: [sessionId] }),
      diagRequest({ sessionFilterIds: [sessionId, sessionId] }),
      diagRequest({ sessionFilterIds: [""] }),
    ]) {
      await expect(handlers.diagSnapshot(request, deviceContext()))
        .rejects.toMatchObject({ code: Code.InvalidArgument });
    }
    expect(worker.sentWorkerFps).toEqual([]);
    expect(worker.pipelineTargetsByWorker).toEqual({});
  });

  test("caps the unfiltered fleet dump and marks it truncated", async () => {
    const snapshot = snapshotPayload(
      await systemHandlers().diagSnapshot(diagRequest(), deviceContext()),
    );

    const openSessionIds = [...BATCH_SESSION_IDS, LOCAL_UNSELECTED_SESSION];
    const returned = Object.keys(snapshot.coord.sessions);
    expect(returned.length).toBe(64);
    expect(returned.length).toBeLessThan(openSessionIds.length);
    for (const sessionId of returned) expect(openSessionIds).toContain(sessionId);
    expect(snapshot.truncated).toBe(true);
    expect(Object.keys(snapshot.workers).sort()).toEqual([WORKER_A, WORKER_C, WORKER_LOCAL]);
    expect(worker.sentWorkerFps.sort()).toEqual([WORKER_A, WORKER_C, WORKER_LOCAL]);
    expect(returnedPipelineSessionIds(snapshot).length).toBe(returned.length);
  });
});

describe("DiagSnapshot terminal capture dispatch", () => {
  test("requires exactly one session_filter_ids entry naming the capture session", async () => {
    const handlers = systemHandlers();
    for (const request of [
      diagRequest({ terminalCapture: captureMessage() }),
      diagRequest({ sessionFilterIds: [MISSING_SESSION], terminalCapture: captureMessage() }),
      diagRequest({
        sessionFilterIds: [CAPTURE_SESSION, BATCH_SESSION_IDS[1]!],
        terminalCapture: captureMessage(),
      }),
      // The legacy scalar filter cannot express the one-session scope a capture
      // request needs, so it is refused rather than reinterpreted.
      diagRequest({ sessionFilterId: CAPTURE_SESSION, terminalCapture: captureMessage() }),
    ]) {
      await expect(handlers.diagSnapshot(request, deviceContext()))
        .rejects.toMatchObject({ code: Code.InvalidArgument });
    }
    expect(worker.capture.commands).toEqual([]);
    expect(worker.sentWorkerFps).toEqual([]);
  });

  test("rejects an unauthenticated caller before any capture work", async () => {
    await expect(systemHandlers().diagSnapshot(
      diagRequest({ sessionFilterIds: [CAPTURE_SESSION], terminalCapture: captureMessage() }),
      anonymousContext(),
    )).rejects.toMatchObject({ code: Code.Unauthenticated });
    expect(worker.capture.commands).toEqual([]);
  });

  test("a session the caller cannot reach never reaches the worker", async () => {
    await expect(systemHandlers().diagSnapshot(
      diagRequest({
        sessionFilterIds: [MISSING_SESSION],
        terminalCapture: captureMessage({ sessionId: MISSING_SESSION }),
      }),
      deviceContext(),
    )).rejects.toMatchObject({
      code: Code.NotFound,
      rawMessage: "session_unknown: session_id",
    });
    expect(worker.capture.commands).toEqual([]);
  });

  test("oversized, malformed, sectionless and cross-session evidence is refused before dispatch", async () => {
    const handlers = systemHandlers();
    const captureWith = (evidence: string) => diagRequest({
      sessionFilterIds: [CAPTURE_SESSION],
      terminalCapture: captureMessage({
        action: TerminalCaptureAction.CAPTURE,
        browserEvidenceJson: evidence,
      }),
    });
    await expect(handlers.diagSnapshot(
      captureWith("x".repeat(TERMINAL_CAPTURE_LIMITS.browserEvidenceBytes + 1)),
      deviceContext(),
    )).rejects.toMatchObject({
      code: Code.InvalidArgument,
      rawMessage: "evidence_too_large: browser_evidence_json",
    });
    await expect(handlers.diagSnapshot(captureWith("{not-json"), deviceContext()))
      .rejects.toMatchObject({
        code: Code.InvalidArgument,
        rawMessage: "evidence_malformed: browser_evidence_json",
      });
    await expect(handlers.diagSnapshot(
      captureWith(JSON.stringify({ ...capturePayload(), layer: "worker" })),
      deviceContext(),
    )).rejects.toMatchObject({
      code: Code.InvalidArgument,
      rawMessage: "evidence_malformed: layer",
    });
    // A payload that names this capture but carries no nested `browser`
    // section is the production silent-drop: it must fail here, loudly.
    await expect(handlers.diagSnapshot(
      captureWith(JSON.stringify(withoutLayerSection(capturePayload()))),
      deviceContext(),
    )).rejects.toMatchObject({
      code: Code.InvalidArgument,
      rawMessage: "evidence_malformed: browser",
    });
    await expect(handlers.diagSnapshot(
      captureWith(captureEvidence(LOCAL_UNSELECTED_SESSION)),
      deviceContext(),
    )).rejects.toMatchObject({
      code: Code.PermissionDenied,
      rawMessage: "permission_denied: session_id",
    });
    expect(worker.capture.commands).toEqual([]);
  });

  test("answers with the capture result alone and logs no terminal content", async () => {
    const handlers = systemHandlers();
    const armed = await handlers.diagSnapshot(
      diagRequest({ sessionFilterIds: [CAPTURE_SESSION], terminalCapture: captureMessage() }),
      deviceContext(),
    );
    expect(captureResultOf(armed).capture).toMatchObject({
      session_id: CAPTURE_SESSION,
      recording_id: CAPTURE_RECORDING,
      action: "start",
      status: "recording",
      worker_fp: WORKER_A,
    });

    const captured = await handlers.diagSnapshot(
      diagRequest({
        sessionFilterIds: [CAPTURE_SESSION],
        terminalCapture: captureMessage({
          action: TerminalCaptureAction.CAPTURE,
          browserEvidenceJson: captureEvidence(),
        }),
      }),
      deviceContext(),
    );
    const { payload, capture } = captureResultOf(captured);
    // A capture answer carries the result and nothing that could hold terminal
    // state: no coord dump, no worker fan-out, no SPA echo.
    expect(Object.keys(payload).sort()).toEqual(["captured_at_ms", "terminal_capture"]);
    expect(capture).toMatchObject({ action: "capture", status: "captured", byte_length: 4_096 });
    expect(worker.sentWorkerFps).toEqual([]);
    // The browser's evidence reaches the worker's owner-only bundle, and
    // nothing else.
    expect(String(worker.capture.commands[1]!.browser_evidence_json)).toContain(EVIDENCE_MARKER);
    expect(captured.snapshotJson).not.toContain(EVIDENCE_MARKER);
    expect(JSON.stringify(emittedSignals)).not.toContain(EVIDENCE_MARKER);
    expect(emittedSignals.map((record) => record.evt)).toEqual(["terminal.capture_started"]);
    // Both layers cross the wire nested under their own layer member; the
    // worker unwraps `coordinator`/`browser`, never the envelope itself.
    const forwarded = JSON.parse(
      String(worker.capture.commands[1]!.coordinator_evidence_json),
    ) as Record<string, unknown>;
    expect(forwarded).toMatchObject({
      schema: TERMINAL_INCIDENT_SCHEMA,
      layer: "coordinator",
      capture_id: CAPTURE_ID,
      session_id: CAPTURE_SESSION,
    });
    expect(forwarded.coordinator).toMatchObject({ layer: "coordinator" });
    expect(forwarded.records).toBeUndefined();
  });

  test("a worker that drops the capture returns a fixed error code, not a throw", async () => {
    worker.capture.replies = [{ kind: "drop" }];
    const response = await systemHandlers().diagSnapshot(
      diagRequest({ sessionFilterIds: [CAPTURE_SESSION], terminalCapture: captureMessage() }),
      deviceContext(),
    );
    expect(captureResultOf(response).capture).toMatchObject({
      status: "error",
      error: "worker_offline",
      path: null,
      expires_at_ms: null,
    });
  });

  test("an ordinary snapshot keeps its own projection", async () => {
    const response = await systemHandlers().diagSnapshot(
      diagRequest({ sessionFilterIds: [CAPTURE_SESSION] }),
      deviceContext(),
    );
    const payload = JSON.parse(response.snapshotJson!) as Record<string, unknown>;
    expect(Object.keys(payload).sort()).toEqual(["captured_at_ms", "coord", "spa", "workers"]);
    expect(payload.terminal_capture).toBeUndefined();
    expect(worker.sentWorkerFps).toEqual([WORKER_A]);
    expect(worker.capture.commands).toEqual([]);
  });
});
