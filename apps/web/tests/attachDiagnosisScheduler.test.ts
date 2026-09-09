// Document attach-diagnosis scheduler coverage: bounded batches and cancellation.
// The transport fixture intentionally holds requests open to prove global serialization.
// Terminal presentation recovery is exercised beside a pending diagnosis request.

import { afterEach, describe, expect, test, vi } from "bun:test";
import { createRoot, createSignal } from "solid-js";
import type { CellGridRenderer } from "../src/lib/cellRenderer.ts";
import {
  ATTACH_DIAGNOSIS_BATCH_INTERVAL_MS,
  ATTACH_DIAGNOSIS_BATCH_MAX_SESSIONS,
  createAttachDiagnosisScheduler,
  type AttachDiagnosisBatchResponse,
} from "../src/lib/attachDiagnosisScheduler.ts";
import {
  createTerminalPresentationController,
  FOREGROUND_DOM_STALL_MS,
} from "../src/lib/terminalPresentation.ts";
import type { TerminalViewHandleStatus } from "../src/store/terminal-stream-types.ts";

const acceptedView: TerminalViewHandleStatus = {
  status: "accepted",
  revision: 1n,
  active: true,
  streamId: "10000000-0000-4000-8000-000000000001",
  effectiveCols: 80,
  effectiveRows: 24,
  baselineReady: true,
};

function sessionIds(count: number): string[] {
  return Array.from({ length: count }, (_, index) =>
    `00000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`,
  );
}

function diagnosisSnapshot(
  ids: readonly string[],
  workerGate = false,
): AttachDiagnosisBatchResponse {
  const sessions = Object.fromEntries(ids.map((sessionId) => [sessionId, {
    route: { connected: true, worker_fp: "worker" },
    terminal_view: { unavailable: false },
    terminal_screen: { seq: "1", valid: true },
  }]));
  const workerSessions = workerGate
    ? Object.fromEntries(ids.map((sessionId) => [sessionId, {
      gate: { active: true, gate: "baseline", age_ms: 500 },
    }]))
    : {};
  return {
    snapshotJson: JSON.stringify({
      coord: { sessions },
      workers: workerGate
        ? { worker: { status: "ok", snapshot: { sessions: workerSessions } } }
        : {},
    }),
  };
}

async function flushDiagnosisWork(): Promise<void> {
  for (let round = 0; round < 4; round += 1) await Promise.resolve();
}

afterEach(() => vi.useRealTimers());

describe("attach diagnosis document scheduler", () => {
  test("batches at most 64 waiting sessions and rotates to the next cohort", async () => {
    vi.useFakeTimers();
    const ids = sessionIds(ATTACH_DIAGNOSIS_BATCH_MAX_SESSIONS * 2);
    const requests: string[][] = [];
    const scheduler = createAttachDiagnosisScheduler((requestedIds) => {
      requests.push([...requestedIds]);
      return Promise.resolve(diagnosisSnapshot(requestedIds));
    });
    try {
      for (const sessionId of ids) scheduler.register(sessionId, () => undefined);
      vi.advanceTimersByTime(1);
      await flushDiagnosisWork();
      expect(requests).toEqual([ids.slice(0, ATTACH_DIAGNOSIS_BATCH_MAX_SESSIONS)]);

      vi.advanceTimersByTime(ATTACH_DIAGNOSIS_BATCH_INTERVAL_MS);
      await flushDiagnosisWork();
      expect(requests).toEqual([
        ids.slice(0, ATTACH_DIAGNOSIS_BATCH_MAX_SESSIONS),
        ids.slice(ATTACH_DIAGNOSIS_BATCH_MAX_SESSIONS),
      ]);
    } finally {
      scheduler.dispose();
    }
  });

  test("keeps one RPC in flight even after later batch deadlines pass", async () => {
    vi.useFakeTimers();
    const ids = sessionIds(ATTACH_DIAGNOSIS_BATCH_MAX_SESSIONS + 1);
    const firstResponse = Promise.withResolvers<AttachDiagnosisBatchResponse>();
    let requestCount = 0;
    const scheduler = createAttachDiagnosisScheduler((requestedIds) => {
      requestCount += 1;
      return requestCount === 1
        ? firstResponse.promise
        : Promise.resolve(diagnosisSnapshot(requestedIds));
    });
    try {
      for (const sessionId of ids) scheduler.register(sessionId, () => undefined);
      vi.advanceTimersByTime(1);
      await flushDiagnosisWork();
      expect(requestCount).toBe(1);

      vi.advanceTimersByTime(ATTACH_DIAGNOSIS_BATCH_INTERVAL_MS * 2);
      await flushDiagnosisWork();
      expect(requestCount).toBe(1);

      firstResponse.resolve(diagnosisSnapshot(ids.slice(0, ATTACH_DIAGNOSIS_BATCH_MAX_SESSIONS)));
      await flushDiagnosisWork();
      vi.advanceTimersByTime(1);
      await flushDiagnosisWork();
      expect(requestCount).toBe(2);
    } finally {
      scheduler.dispose();
    }
  });

  test("aborts and suppresses a disposed registration immediately", async () => {
    vi.useFakeTimers();
    const [sessionId] = sessionIds(1);
    const deferredResponse = Promise.withResolvers<AttachDiagnosisBatchResponse>();
    let requestSignal: AbortSignal | null = null;
    let requestCount = 0;
    const reasons: Array<string | null> = [];
    const scheduler = createAttachDiagnosisScheduler((_requestedIds, signal) => {
      requestCount += 1;
      requestSignal = signal;
      return deferredResponse.promise;
    });
    try {
      const registration = scheduler.register(sessionId!, (reason) => reasons.push(reason));
      vi.advanceTimersByTime(1);
      expect(requestSignal).not.toBeNull();
      const activeRequestSignal = requestSignal as unknown as AbortSignal;
      expect(activeRequestSignal.aborted).toBe(false);

      registration.dispose();
      expect(activeRequestSignal.aborted).toBe(true);
      deferredResponse.resolve(diagnosisSnapshot([sessionId!], true));
      await flushDiagnosisWork();
      vi.advanceTimersByTime(ATTACH_DIAGNOSIS_BATCH_INTERVAL_MS);
      await flushDiagnosisWork();
      expect(requestCount).toBe(1);
      expect(reasons).toEqual([]);
    } finally {
      scheduler.dispose();
    }
  });

  test("does not deliver the implicit empty diagnosis state", async () => {
    vi.useFakeTimers();
    const [sessionId] = sessionIds(1);
    const reasons: Array<string | null> = [];
    const scheduler = createAttachDiagnosisScheduler(() =>
      Promise.resolve(diagnosisSnapshot([sessionId!])),
    );
    try {
      scheduler.register(sessionId!, (reason) => reasons.push(reason));
      vi.advanceTimersByTime(1);
      await flushDiagnosisWork();
      expect(reasons).toEqual([]);
    } finally {
      scheduler.dispose();
    }
  });

  test("delivers a diagnosis transition once and clears it once", async () => {
    vi.useFakeTimers();
    const [sessionId] = sessionIds(1);
    const responses = [
      diagnosisSnapshot([sessionId!], true),
      diagnosisSnapshot([sessionId!], true),
      diagnosisSnapshot([sessionId!]),
    ];
    const reasons: Array<string | null> = [];
    const scheduler = createAttachDiagnosisScheduler(() =>
      Promise.resolve(responses.shift() ?? diagnosisSnapshot([sessionId!])),
    );
    try {
      scheduler.register(sessionId!, (reason) => reasons.push(reason));
      vi.advanceTimersByTime(1);
      await flushDiagnosisWork();
      vi.advanceTimersByTime(ATTACH_DIAGNOSIS_BATCH_INTERVAL_MS);
      await flushDiagnosisWork();
      vi.advanceTimersByTime(ATTACH_DIAGNOSIS_BATCH_INTERVAL_MS);
      await flushDiagnosisWork();
      expect(reasons).toEqual(["Building baseline (1s)", null]);
    } finally {
      scheduler.dispose();
    }
  });

  test("does not gate scoped recovery while diagnosis remains in flight", async () => {
    vi.useFakeTimers();
    const [sessionId] = sessionIds(1);
    const pendingDiagnosis = Promise.withResolvers<AttachDiagnosisBatchResponse>();
    const scheduler = createAttachDiagnosisScheduler(() => pendingDiagnosis.promise);
    const [active] = createSignal(true);
    const renderer = {
      canonicalEpochSeq: () => ({ grid_epoch: "epoch-a", seq: 2 }),
      reconciledEpochSeq: () => ({ grid_epoch: "epoch-a", seq: 1 }),
      setCursorBlinkEnabled: () => undefined,
    } as unknown as CellGridRenderer;
    let recoveryCalls = 0;
    let disposeRoot = (): void => undefined;
    const presentation = createRoot((rootDispose) => {
      disposeRoot = rootDispose;
      const presentationOptions = {
        active,
        focused: () => true,
        status: () => acceptedView,
        renderer: () => renderer,
        onCatchUpStalled: () => { recoveryCalls += 1; },
      };
      return createTerminalPresentationController(presentationOptions);
    });
    try {
      scheduler.register(sessionId!, () => undefined);
      presentation.refreshTerminalPresentation();
      vi.advanceTimersByTime(FOREGROUND_DOM_STALL_MS);
      expect(recoveryCalls).toBe(1);
    } finally {
      disposeRoot();
      scheduler.dispose();
      pendingDiagnosis.resolve(diagnosisSnapshot([sessionId!]));
      await flushDiagnosisWork();
    }
  });
});
