// Attach-stall diagnosis mapping: coordinator snapshots → one loading-card line.
// The canned snapshots mirror the coordinator's bounded session response.
// attachDiagnosisScheduler.test.ts owns cadence, cancellation, and batch tests.
import {
  ATTACH_DIAGNOSIS_BATCH_INTERVAL_MS,
  _resetAttachDiagnosisSchedulerForTest,
  attachDiagnosisReasonFromSnapshot,
  attachDiagnosisWaitKey,
  startAttachDiagnosis,
} from "../src/lib/attachDiagnosis.ts";
import { afterEach, beforeEach, describe, expect, test, vi, mock } from "bun:test";

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
          [`${"f".repeat(64)}`]: {
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

  test("ignores a stale worker gate after session reassignment", () => {
    const outcome = attachDiagnosisReasonFromSnapshot({
      coord: {
        sessions: {
          [SID]: coordSession({ terminal_screen: null }),
        },
      },
      workers: {
        ["a".repeat(64)]: {
          status: "ok",
          snapshot: {
            sessions: {
              [SID]: {
                gate: {
                  active: true,
                  gate: "baseline",
                  age_ms: 500,
                },
              },
            },
          },
        },
        ["f".repeat(64)]: {
          status: "ok",
          snapshot: { sessions: {} },
        },
      },
    }, SID, { previousTerminalScreenSeq: null });
    expect(outcome.reason).toBeNull();
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
  let requestedSessionIds: string[][];

  const flush = async (): Promise<void> => {
    for (let round = 0; round < 4; round += 1) await Promise.resolve();
  };

  beforeEach(() => {
    vi.useFakeTimers();
    _resetAttachDiagnosisSchedulerForTest();
    diagSnapshots = [];
    requestedSessionIds = [];
    mock.module("../src/connect.ts", () => ({
      coordClient: {
        diagSnapshot(request: { sessionFilterIds: string[] }) {
          requestedSessionIds.push([...request.sessionFilterIds]);
          const next = diagSnapshots.shift();
          if (next instanceof Error) return Promise.reject(next);
          return Promise.resolve(next ?? { snapshotJson: "{}" });
        },
      },
    }));
  });
  afterEach(() => {
    _resetAttachDiagnosisSchedulerForTest();
    vi.useRealTimers();
  });

  test("registers through the bounded batch request and suppresses unchanged delivery", async () => {
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
    diagSnapshots = [gated(), gated()];
    const reasons: Array<string | null> = [];
    const handle = startAttachDiagnosis(SID, (reason) => reasons.push(reason));
    vi.advanceTimersByTime(1);
    await flush();
    expect(requestedSessionIds).toEqual([[SID]]);
    expect(reasons).toEqual(["Building baseline (1s)"]);

    vi.advanceTimersByTime(ATTACH_DIAGNOSIS_BATCH_INTERVAL_MS);
    await flush();
    expect(requestedSessionIds).toEqual([[SID], [SID]]);
    expect(reasons).toEqual(["Building baseline (1s)"]);
    handle.dispose();
  });
});

describe("attach diagnosis grace", () => {
  test("restarts for every accepted progress update and clears when inactive", () => {
    const beforeProgress = {
      snapshotId: "10000000-0000-4000-8000-000000000010",
      receivedChunks: 1,
      totalChunks: 3,
    };
    const afterProgress = {
      ...beforeProgress,
      receivedChunks: 2,
    };
    expect(attachDiagnosisWaitKey("frame", beforeProgress))
      .not.toBe(attachDiagnosisWaitKey("frame", afterProgress));
    expect(attachDiagnosisWaitKey("frame", afterProgress)).not.toBeNull();
    expect(attachDiagnosisWaitKey(null, afterProgress)).toBeNull();
  });
});
