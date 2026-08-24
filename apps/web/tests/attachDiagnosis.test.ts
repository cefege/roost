// Attach-stall diagnosis mapping: coordinator DiagSnapshot wire shapes → one
// human "stuck here" line for the loading card. The canned snapshots mirror
// the real dump exactly — coord.sessions[<id>] from handlers-system's
// session-scoped branch, workers[<fp>] from collectWorkerDiagSnapshots with
// the worker-side session-lifecycle.diagSnapshot payload inside `.snapshot`.
// The polling loop is driven through startAttachDiagnosis with a mocked
// coordClient (same mock.module pattern as attachmentsPicker.dom.test.ts).
import {
  ATTACH_DIAGNOSIS_POLL_MS,
  attachDiagnosisReasonFromSnapshot,
  startAttachDiagnosis,
} from "../src/lib/attachDiagnosis.ts";
import { describe, expect, test, beforeEach, afterEach, vi, mock } from "bun:test";

const SID = "00000000-0000-4000-8000-00000000d001";

interface CoordSessionShape {
  route: Record<string, unknown> | null;
  terminal_view: Record<string, unknown> | null;
  terminal_screen: Record<string, unknown> | null;
}

function coordSession(overrides: Partial<CoordSessionShape> = {}): CoordSessionShape {
  return {
    route: {
      worker_fp: "f".repeat(64),
      channel_id: 7,
      connected: true,
      source: "live_cache",
    },
    terminal_view: {
      activeViews: 1,
      parkedViews: 0,
      streamId: "00000000-0000-4000-8000-00000000e001",
      effective: { cols: 80, rows: 24 },
      unavailable: false,
    },
    terminal_screen: {
      stream_id: "00000000-0000-4000-8000-00000000e001",
      grid_epoch: "g1",
      seq: "12",
      cols: 80,
      rows: 24,
      valid: true,
    },
    ...overrides,
  };
}

function snapshot(
  session: CoordSessionShape | null,
  workerSessions: Record<string, unknown> = {},
): unknown {
  return {
    captured_at_ms: 1000,
    coord: {
      build: {},
      sessions: session === null ? {} : { [SID]: session },
      agent_status: {},
      terminal_control: {},
    },
    workers: Object.keys(workerSessions).length === 0
      ? {}
      : {
          [`${"a".repeat(64)}`]: {
            status: "ok",
            response_ms: 4,
            snapshot: { sessions: workerSessions },
          },
        },
    spa: null,
  };
}

describe("attachDiagnosisReasonFromSnapshot", () => {
  test("missing route reads as an offline worker", () => {
    const outcome = attachDiagnosisReasonFromSnapshot(
      snapshot(coordSession({ route: null })), SID, { previousTerminalScreenSeq: null },
    );
    expect(outcome.reason).toBe("Worker offline — waiting for it to reconnect");
  });

  test("disconnected route wins over everything else", () => {
    const outcome = attachDiagnosisReasonFromSnapshot(
      snapshot(coordSession({
        route: { worker_fp: "f".repeat(64), channel_id: 7, connected: false, source: "live_cache" },
        terminal_screen: null,
      })), SID, { previousTerminalScreenSeq: null },
    );
    expect(outcome.reason).toBe("Worker offline — waiting for it to reconnect");
  });

  test("coordinator-unavailable view maps to the coordinator line", () => {
    const outcome = attachDiagnosisReasonFromSnapshot(
      snapshot(coordSession({
        terminal_view: {
          activeViews: 0, parkedViews: 0,
          streamId: "00000000-0000-4000-8000-00000000e001",
          effective: null, unavailable: true,
        },
      })), SID, { previousTerminalScreenSeq: null },
    );
    expect(outcome.reason).toBe("Coordinator: terminal view marked unavailable");
  });

  test("invalid screen maps to the resync-repair line", () => {
    const outcome = attachDiagnosisReasonFromSnapshot(
      snapshot(coordSession({
        terminal_screen: {
          stream_id: "00000000-0000-4000-8000-00000000e001", grid_epoch: "g1",
          seq: "30", cols: 80, rows: 24, valid: false,
        },
      })), SID, { previousTerminalScreenSeq: "29" },
    );
    expect(outcome.reason).toBe("Repairing the stream (resync requested)");
  });

  test("worker baseline gate maps to Building baseline with age seconds", () => {
    const outcome = attachDiagnosisReasonFromSnapshot(
      snapshot(coordSession({ terminal_screen: null }), {
        [SID]: {
          gate: {
            active: true, gate: "baseline", age_ms: 2500,
            suppressed_frames: 4, over_budget: false, budget_ms: 7500, reason: null,
          },
        },
      }), SID, { previousTerminalScreenSeq: null },
    );
    expect(outcome.reason).toBe("Building baseline (3s)");
  });

  test("worker resize_capture gate maps to Resizing grid", () => {
    const outcome = attachDiagnosisReasonFromSnapshot(
      snapshot(coordSession(), {
        [SID]: {
          gate: {
            active: true, gate: null, age_ms: 900,
            suppressed_frames: 1, over_budget: false, budget_ms: 7500,
            reason: "resize_capture",
          },
        },
      }), SID, { previousTerminalScreenSeq: null },
    );
    expect(outcome.reason).toBe("Resizing grid (1s)");
  });

  test("worker synchronized-output hold names the buffering app", () => {
    const outcome = attachDiagnosisReasonFromSnapshot(
      snapshot(coordSession(), {
        [SID]: {
          gate: {
            active: true, gate: "sync_output", age_ms: 120,
            suppressed_frames: 8, over_budget: false, budget_ms: 7500,
            reason: "sync_output",
          },
        },
      }), SID, { previousTerminalScreenSeq: "40" },
    );
    expect(outcome.reason).toBe(
      "App is buffering output (synchronized output) (0s)",
    );
  });

  test("advancing screen seq while unpainted reads as local assembly", () => {
    const outcome = attachDiagnosisReasonFromSnapshot(
      snapshot(coordSession()), SID, { previousTerminalScreenSeq: "11" },
    );
    expect(outcome.reason).toBe("Frames flowing — assembling on this device");
  });

  test("nothing wrong clears the line and remembers the seq", () => {
    const outcome = attachDiagnosisReasonFromSnapshot(
      snapshot(coordSession()), SID, { previousTerminalScreenSeq: "12" },
    );
    expect(outcome.reason).toBeNull();
    expect(outcome.terminalScreenSeq).toBe("12");
  });

  test("an unknown session entry stays silent instead of guessing", () => {
    const outcome = attachDiagnosisReasonFromSnapshot(
      snapshot(null), SID, { previousTerminalScreenSeq: "9" },
    );
    expect(outcome.reason).toBeNull();
  });
});

describe("startAttachDiagnosis", () => {
  let diagSnapshots: Array<{ snapshotJson: string } | Error>;
  let diagCalls: string[];

  // Microtasks between timer ticks carry each async poll: promise → parse →
  // onReason → finally.
  const flush = async (): Promise<void> => {
    for (let round = 0; round < 3; round++) await Promise.resolve();
  };

  beforeEach(() => {
    vi.useFakeTimers();
    diagCalls = [];
    // Replaces connect.ts's coordClient live binding before any poll runs
    // (same mock.module pattern as attachmentsPicker.dom.test.ts).
    mock.module("../src/connect.ts", () => ({
      coordClient: {
        diagSnapshot(_request: unknown) {
          diagCalls.push("poll");
          const next = diagSnapshots.shift();
          if (next instanceof Error) return Promise.reject(next);
          return Promise.resolve(next ?? { snapshotJson: "{}" });
        },
      },
    }));
  });
  afterEach(() => vi.useRealTimers());

  test("maps the first poll, keeps polling at 750ms, stops on dispose", async () => {
    const gated = (): { snapshotJson: string } => ({
      snapshotJson: JSON.stringify(snapshot(coordSession({ terminal_screen: null }), {
        [SID]: {
          gate: {
            active: true, gate: "baseline", age_ms: 500,
            suppressed_frames: 2, over_budget: false, budget_ms: 7500, reason: null,
          },
        },
      })),
    });
    // Two polls' worth: one consumed immediately, one at the first 750ms tick.
    diagSnapshots = [gated(), gated()];
    const reasons: Array<string | null> = [];
    const handle = startAttachDiagnosis(SID, (reason) => reasons.push(reason));
    await flush();
    expect(reasons[0]).toBe("Building baseline (1s)");
    expect(diagCalls.length).toBe(1);
    vi.advanceTimersByTime(ATTACH_DIAGNOSIS_POLL_MS);
    await flush();
    expect(diagCalls.length).toBe(2);
    handle.dispose();
    vi.advanceTimersByTime(ATTACH_DIAGNOSIS_POLL_MS * 4);
    await flush();
    expect(diagCalls.length).toBe(2);
    expect(reasons.every((reason) => reason === "Building baseline (1s)")).toBe(true);
  });

  test("a failed poll never surfaces and keeps the loop alive", async () => {
    diagSnapshots = [new TypeError("network failed")];
    const reasons: Array<string | null> = [];
    const handle = startAttachDiagnosis(SID, (reason) => reasons.push(reason));
    await flush();
    expect(reasons.length).toBe(0);
    expect(diagCalls.length).toBe(1);
    vi.advanceTimersByTime(ATTACH_DIAGNOSIS_POLL_MS);
    await flush();
    expect(diagCalls.length).toBe(2);
    handle.dispose();
  });
});
