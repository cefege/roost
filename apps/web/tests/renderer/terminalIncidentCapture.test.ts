// Terminal incident capture recorder — lease control, held evidence and the
// automatic-capture latch.
// The coordinator client is mocked: this tier owns recorder behaviour, while
// cellRenderer.incidentHistory.dom.test.ts owns the painted-DOM invariants,
// terminalIncidentProducer.dom.test.ts owns the emitted signals and envelope,
// and terminalIncidentEvidenceBudget.test.ts owns the 512 KiB trimming policy.

import { beforeEach, describe, expect, mock, test } from "bun:test";
import {
  TERMINAL_CAPTURE_LIMITS,
  type TerminalCaptureResult,
} from "@roost/protocol/terminal-capture";

interface DiagSnapshotCall {
  sessionFilterIds?: string[];
  spaStateJson?: string;
  terminalCapture?: {
    action: number;
    sessionId: string;
    captureId: string;
    recordingId: string;
    reason: string;
    browserEvidenceJson: string;
  };
}

const calls: DiagSnapshotCall[] = [];
let nextResult: Partial<TerminalCaptureResult> = {};

// The whole module is replaced, so every export the store graph reaches for
// must exist; only diagSnapshot carries behaviour for these cases.
mock.module("../../src/client/rpc/connect.ts", () => ({
  classifyAuthFailure: () => "retryable",
  coordBase: () => "http://coord.test",
  coordinatorBaseUrl: () => "http://coord.test",
  coordinatorRpcUrl: (path: string) => `http://coord.test${path}`,
  reconcileCoordinatorOverrideAfterDiscovery: () => false,
  makeCoordinatorClientForSigner: () => ({}),
  publicCoordClient: {},
  coordClient: {
    async diagSnapshot(request: DiagSnapshotCall) {
      calls.push(request);
      const capture = request.terminalCapture;
      const result: TerminalCaptureResult = {
        capture_id: capture?.captureId ?? "",
        recording_id: capture?.recordingId ?? "",
        session_id: capture?.sessionId ?? "",
        action: "start",
        status: "recording",
        expires_at_ms: Date.now() + TERMINAL_CAPTURE_LIMITS.leaseMs,
        worker_fp: "worker-fp",
        path: null,
        byte_length: null,
        error: null,
        recent_worker_capture: null,
        ...nextResult,
      };
      return { snapshotJson: JSON.stringify({ captured_at_ms: 1, terminal_capture: result }) };
    },
  },
}));

// Deferred imports: the recorder binds coordClient at module evaluation, so the
// mock above must be registered before the module graph loads.
const {
  captureTerminalIncident,
  captureTerminalIncidentFrozen,
  disposeTerminalIncidentRecorder,
  freezeTerminalCaptureEvidence,
  startTerminalCapture,
  stopTerminalCapture,
  subscribeTerminalCaptureUiState,
  terminalCaptureUiState,
} = await import("../../src/renderer/terminalIncidentCapture.ts");
const {
  _resetTerminalIncidentRecorders,
  admitAutomaticCapture,
  anyTerminalRecorderArmed,
  armTerminalRecorder,
  ensureTerminalRecorder,
  terminalRecorder,
} = await import("../../src/renderer/terminalIncidentCaptureState.ts");

const SESSION = "22222222-2222-4222-8222-222222222222";

function reset(): void {
  calls.length = 0;
  nextResult = {};
  _resetTerminalIncidentRecorders();
}

describe("terminal incident capture — lease control", () => {
  beforeEach(reset);

  test("START arms one acknowledged recording and renews the same identity", async () => {
    const seen: string[] = [];
    const unsubscribe = subscribeTerminalCaptureUiState(SESSION, (state) => seen.push(state.phase));

    const first = await startTerminalCapture(SESSION);
    expect(first.status).toBe("recording");
    const armedState = terminalCaptureUiState(SESSION);
    expect(armedState.phase).toBe("recording");
    expect(armedState.expiresAtMs).not.toBeNull();
    expect(calls[0]?.sessionFilterIds).toEqual([SESSION]);
    expect(calls[0]?.terminalCapture?.browserEvidenceJson).toBe("");
    expect(calls[0]?.terminalCapture?.reason).toBe("manual");

    const second = await startTerminalCapture(SESSION);
    expect(second.recording_id).toBe(first.recording_id);
    expect(calls).toHaveLength(2);
    expect(seen).toContain("arming");
    expect(seen.at(-1)).toBe("recording");
    unsubscribe();
  });

  test("a refused START leaves the pane unarmed and reports the fixed code", async () => {
    nextResult = { status: "error", error: "lease_conflict" };
    const result = await startTerminalCapture(SESSION);
    expect(result.error).toBe("lease_conflict");
    expect(terminalCaptureUiState(SESSION).phase).toBe("error");
    expect(anyTerminalRecorderArmed()).toBe(false);
  });

  test("an invalid session id never reaches the coordinator", async () => {
    const result = await startTerminalCapture("not-a-uuid");
    expect(result.error).toBe("invalid_argument");
    expect(calls).toHaveLength(0);
  });

  test("STOP frees the lease and is idempotent", async () => {
    await startTerminalCapture(SESSION);
    nextResult = { action: "stop", status: "stopped" };
    const stopped = await stopTerminalCapture(SESSION);
    expect(stopped.status).toBe("stopped");
    expect(anyTerminalRecorderArmed()).toBe(false);
    expect(terminalCaptureUiState(SESSION).phase).toBe("idle");

    const callsAfterStop = calls.length;
    const again = await stopTerminalCapture(SESSION);
    expect(again.status).toBe("stopped");
    expect(calls).toHaveLength(callsAfterStop);
  });

  test("an expired lease disarms itself instead of silently renewing", async () => {
    await startTerminalCapture(SESSION);
    const recorder = terminalRecorder(SESSION)!;
    recorder.expiresAtMs = Date.now() - 1;
    expect(terminalCaptureUiState(SESSION).phase).toBe("expired");
    expect(anyTerminalRecorderArmed()).toBe(false);
  });

  test("disposal frees every owned recorder resource", async () => {
    await startTerminalCapture(SESSION);
    const recorder = terminalRecorder(SESSION)!;
    recorder.events.push({
      at_ms: 1,
      kind: "render_applied",
      stream: null,
      apply_mode: "full",
      detail: null,
    });
    disposeTerminalIncidentRecorder(SESSION);

    expect(terminalRecorder(SESSION)).toBeUndefined();
    expect(anyTerminalRecorderArmed()).toBe(false);
    expect(recorder.events).toHaveLength(0);
    expect(recorder.renewTimer).toBeNull();
    expect(recorder.listeners.size).toBe(0);
    expect(terminalCaptureUiState(SESSION).phase).toBe("idle");
  });
});

describe("terminal incident capture — evidence retention", () => {
  beforeEach(reset);

  test("a failed upload holds its frozen payload and a retry reuses the capture id", async () => {
    await startTerminalCapture(SESSION);
    nextResult = { action: "capture", status: "error", error: "worker_offline" };

    const failed = await captureTerminalIncident(SESSION, "manual");
    expect(failed.error).toBe("worker_offline");
    expect(terminalCaptureUiState(SESSION).heldEvidence).toBe(true);
    const firstCaptureId = calls.at(-1)?.terminalCapture?.captureId;

    const token = freezeTerminalCaptureEvidence(SESSION, "manual");
    terminalRecorder(SESSION)!.lastManualAtMs = 0;
    nextResult = { action: "capture", status: "captured", path: "/tmp/x.json.gz", byte_length: 12 };
    const retried = await captureTerminalIncidentFrozen(token);

    expect(retried.status).toBe("captured");
    expect(calls.at(-1)?.terminalCapture?.captureId).toBe(firstCaptureId!);
    expect(terminalCaptureUiState(SESSION).heldEvidence).toBe(false);
  });

  test("manual capture is rate limited without issuing an RPC", async () => {
    await startTerminalCapture(SESSION);
    nextResult = { action: "capture", status: "captured" };
    await captureTerminalIncident(SESSION, "manual");
    const sent = calls.length;

    const limited = await captureTerminalIncident(SESSION, "manual");
    expect(limited.error).toBe("rate_limited");
    expect(calls).toHaveLength(sent);
  });
});

describe("terminal incident capture — automatic admission", () => {
  beforeEach(reset);

  function armedRecorder() {
    const recorder = ensureTerminalRecorder(SESSION, "rec-1");
    armTerminalRecorder(recorder, Date.now() + TERMINAL_CAPTURE_LIMITS.leaseMs);
    return recorder;
  }

  test("one capture per identity, and a new epoch does not buy a new budget", () => {
    const recorder = armedRecorder();
    const now = 1_000_000;

    const first = admitAutomaticCapture(recorder, "stream-1", "epoch-1", "history_identity", now);
    expect(first.allowed).toBe(true);
    expect(first.occurrences).toBe(1);

    const repeat = admitAutomaticCapture(recorder, "stream-1", "epoch-1", "history_identity", now + 5);
    expect(repeat.allowed).toBe(false);
    expect(repeat.occurrences).toBe(2);

    const newEpoch = admitAutomaticCapture(recorder, "stream-1", "epoch-2", "history_identity", now + 10);
    expect(newEpoch.allowed).toBe(false);
    expect(newEpoch.occurrences).toBe(1);

    const afterCooldown = admitAutomaticCapture(
      recorder,
      "stream-1",
      "epoch-2",
      "history_identity",
      now + TERMINAL_CAPTURE_LIMITS.automaticCooldownMs,
    );
    expect(afterCooldown.allowed).toBe(true);

    // The first identity stays latched even once the cooldown has elapsed.
    const latched = admitAutomaticCapture(
      recorder,
      "stream-1",
      "epoch-1",
      "history_identity",
      now + TERMINAL_CAPTURE_LIMITS.automaticCooldownMs * 3,
    );
    expect(latched.allowed).toBe(false);
  });

  test("an in-flight capture blocks another automatic admission", () => {
    const recorder = armedRecorder();
    recorder.captureInFlight = true;
    const blocked = admitAutomaticCapture(recorder, "stream-1", "epoch-1", "viewport_model", 5_000);
    expect(blocked.allowed).toBe(false);
  });

  test("the capture and STOP results report the accumulated conflict count", async () => {
    await startTerminalCapture(SESSION);
    const recorder = terminalRecorder(SESSION)!;
    for (let at = 0; at < 4; at++) {
      admitAutomaticCapture(recorder, "stream-1", "epoch-1", "history_identity", 1_000_000 + at);
    }
    nextResult = { action: "capture", status: "captured", path: "/tmp/x.json.gz", byte_length: 9 };

    const captured = await captureTerminalIncident(SESSION, "history_identity");
    expect(captured.conflicts).toEqual({
      occurrences: 4,
      identities: 1,
      captured: 1,
      dropped_identities: 0,
    });

    // Counters outlive the lease they describe: STOP frees the recorder maps
    // and still reports how often the invariant fired.
    nextResult = { action: "stop", status: "stopped" };
    const stopped = await stopTerminalCapture(SESSION);
    expect(stopped.conflicts.occurrences).toBe(4);
    expect(stopped.conflicts.identities).toBe(1);
    expect(recorder.occurrences.size).toBe(0);
  });
});
