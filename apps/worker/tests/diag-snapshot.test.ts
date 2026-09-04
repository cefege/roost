import { describe, expect, test } from "bun:test";
import { asChannelId, asSessionId, asWorkerFp } from "@roost/shared/wire";
import { initCellEmitState } from "@roost/shared/cell";
import { SessionManager } from "../src/session-manager.ts";
import { getMultiplexedPool } from "../src/keeper/multiplexed-client.ts";
import {
  appendToRing,
  createSbRing,
} from "../src/session-scrollback-ring.ts";
import { monoNowMs } from "../src/util/mono.ts";
import { LifecycleTestSink } from "./lifecycle-test-sink.ts";

const WORKER_FP = asWorkerFp("d".repeat(64));
const SESSION_ID = asSessionId("11111111-2222-4333-8444-555555555555");
const CHANNEL_ID = asChannelId(43_210);

describe("worker diagnostic snapshot", () => {
  test("reports bounded authoritative session state and leaves unknown values explicit", () => {
    const manager = new SessionManager({
      workerFp: WORKER_FP,
      sink: new LifecycleTestSink(),
    });
    const cellEmit = initCellEmitState("diag-grid", "00000000-0000-4000-8000-000000000001");
    cellEmit.seq = 17;
    const scrollback = createSbRing();
    appendToRing(scrollback, Buffer.from("retained"));
    manager.sessions.set(CHANNEL_ID, {
      sessionId: SESSION_ID,
      channelId: CHANNEL_ID,
      kind: "shell",
      cwd: "/tmp/diag",
      session_trace_id: "trace-diag",
      scrollback,
      head_seq: 41,
      alt_mode: false,
      // Enough of a core to be truthful: the snapshot reads the LIVE eviction
      // origin, and a core that cannot report discards is a worker-readiness
      // failure in production, so the stub must not model one that can't.
      wtermCore: {
        getCols: () => 91,
        getRows: () => 27,
        getScrollbackCount: () => 640,
        getScrollbackDiscardedCount: () => 512,
      },
      cell_emit: cellEmit,
      sb_origin_pin: null,
    } as never);
    const baseline = Promise.withResolvers<boolean>();
    manager.terminalStreams.set(CHANNEL_ID, {
      streamId: "00000000-0000-4000-8000-000000000001",
      enabled: true,
      cols: 91,
      rows: 27,
      version: 1,
      baselineReady: false,
      coreValid: true,
      baselineDirty: true,
      snapshotCursor: null,
      baselineInstalled: baseline.promise,
      baselinePromisePending: true,
      resolveBaselineInstalled: baseline.resolve,
      resizeCapture: {
        streamId: "00000000-0000-4000-8000-000000000001",
        resizeSeq: 12,
        installSeq: 36,
        fromCols: 80,
        fromRows: 24,
        toCols: 91,
        toRows: 27,
        queryCarry: new Uint8Array(),
        capturedBytes: 5,
        capturedChunks: 2,
        boundarySeq: 38,
        boundaryApplied: true,
        failedReason: null,
      },
    });
    manager.lastAppliedSize.set(CHANNEL_ID, { cols: 91, rows: 27 });
    manager.channelResizeSeq.set(CHANNEL_ID, 12);
    manager.cellEmissionGates.add(CHANNEL_ID);
    manager.cellGateSuppression.set(CHANNEL_ID, {
      gate: "resize_capture",
      sinceMonoMs: monoNowMs() - 25,
      frames: 3,
      overBudget: false,
      budgetMs: 2_500,
    });
    manager.pendingCellRepairs.add(CHANNEL_ID);
    manager.terminalControlChains.set(CHANNEL_ID, {
      tail: Promise.resolve(),
      depth: 2,
      running: "terminal_stream",
      runningSinceMonoMs: monoNowMs() - 20,
    });
    manager.keeperAdmissionLane.set(CHANNEL_ID, {
      tail: Promise.resolve(),
      depth: 1,
      holder: "terminal_resize",
      heldSinceMonoMs: monoNowMs() - 15,
    });
    manager.rawMetadataQueues.set(CHANNEL_ID, {
      frames: [{ endSeq: 41, bytes: new Uint8Array([1, 2, 3]) }],
      bytes: 3,
    });

    const keeper = getMultiplexedPool();
    const resizeKey = `${CHANNEL_ID}:13`;
    const priorInputUsage = keeper._pendingInputUsage.get(CHANNEL_ID);
    const priorResize = keeper.pendingResizes.get(resizeKey);
    keeper._pendingInputUsage.set(CHANNEL_ID, { commands: 2, bytes: 11 });
    keeper.pendingResizes.set(resizeKey, {
      channelId: CHANNEL_ID,
      seq: 13,
      startedMonoMs: monoNowMs() - 35,
      timer: 0 as never,
      resolve: () => {},
    });

    try {
      const snapshot = manager.diagSnapshot() as {
        build: { git_sha: string; artifact_version: string };
        worker_fp: string;
        sessions: Record<string, {
          channel_binding: unknown;
          raw: unknown;
          cell: unknown;
          gate: {
            active: boolean;
            gate: string | null;
            age_ms: number | null;
            suppressed_frames: number;
            over_budget: boolean;
            budget_ms: number;
            reason: string | null;
          };
          resize_capture: {
            stream_id: string;
            resize_seq: number;
            boundary_applied: boolean;
            captured_bytes: number;
          } | null;
          pending_repair: boolean;
          terminal_stream: {
            stream_id: string;
            baseline_ready: boolean;
            baseline_dirty: boolean;
            core_valid: boolean;
          } | null;
          terminal_control: {
            control_running_age_ms: number | null;
            admission_held_age_ms: number | null;
            resize_ack: { oldest_age_ms: number | null };
          };
        }>;
      };
      const session = snapshot.sessions[SESSION_ID]!;

      expect(() => JSON.stringify(snapshot)).not.toThrow();
      expect(snapshot.worker_fp).toBe(WORKER_FP);
      expect(snapshot.build.git_sha).toBeString();
      expect(snapshot.build.artifact_version).toBeString();
      expect(session.channel_binding).toEqual({
        worker_fp: WORKER_FP,
        channel_id: CHANNEL_ID,
      });
      expect(session.raw).toEqual({
        head_seq: 41,
        tail_seq: 33,
        retained_bytes: 8,
        // The byte ring's own bound, and whether it is currently dropping the
        // oldest bytes to stay inside it — 8 retained of 1 MiB is not.
        cap_bytes: 1_048_576,
        evicting: false,
      });
      expect(session.cell).toEqual({
        grid_epoch: "diag-grid:0",
        seq: 17,
        dirty: false,
        // Frozen at the last successful emit, which is exactly why it is not
        // the number the read paths use.
        sb_dropped: 0,
        last_sb_total: 0,
        // Roost's monotonic base for THIS core instance; nonzero only after a
        // rebuild has pinned one, and origin_pin then says which rebuild.
        sb_origin: 0,
        origin_pin: null,
        // Read LIVE off the core: `discarded` is its own authoritative count,
        // `dropped` is that plus sb_origin (the eviction origin the backfill and
        // search RPCs resolve absolute indices through), and total is the
        // monotonic depth. The gap between `dropped` here and `sb_dropped` above
        // is precisely how far the ring has rolled since the last frame shipped.
        core: {
          discarded: 512,
          dropped: 512,
          retained_lines: 640,
          total: 1_152,
        },
      });
      expect(session.gate).toMatchObject({
        active: true,
        gate: "resize_capture",
        reason: "resize_capture",
        suppressed_frames: 3,
        over_budget: false,
      });
      expect(typeof session.gate.age_ms).toBe("number");
      if (session.gate.age_ms === null) throw new Error("active gate has no age");
      expect(session.gate.age_ms).toBeGreaterThanOrEqual(25);
      expect(session.resize_capture).toMatchObject({
        stream_id: "00000000-0000-4000-8000-000000000001",
        resize_seq: 12,
        install_seq: 36,
        from_cols: 80,
        from_rows: 24,
        to_cols: 91,
        to_rows: 27,
        boundary_seq: 38,
        boundary_applied: true,
        captured_bytes: 5,
        captured_chunks: 2,
        failed_reason: null,
      });
      if (session.resize_capture === null) throw new Error("installed capture is not reported");
      expect(session.pending_repair).toBe(true);
      expect(session.terminal_stream).toMatchObject({
        stream_id: "00000000-0000-4000-8000-000000000001",
        enabled: true,
        cols: 91,
        rows: 27,
        baseline_ready: false,
        baseline_dirty: true,
        core_valid: true,
      });
      expect(session.terminal_control).toMatchObject({
        control_state: "terminal_stream",
        control_depth: 2,
        admission_holder: "terminal_resize",
        admission_depth: 1,
        last_resize_seq: 12,
        input_ack: {
          pending_commands: 2,
          pending_bytes: 11,
        },
        resize_ack: {
          pending_commands: 1,
          min_seq: 13,
          max_seq: 13,
        },
        raw_metadata_queue: {
          pending_frames: 1,
          pending_bytes: 3,
        },
      });
      // Command and lane ages are monotonic durations, never wall-clock deltas.
      const control = session.terminal_control;
      expect(control.control_running_age_ms).toBeGreaterThanOrEqual(20);
      expect(control.admission_held_age_ms).toBeGreaterThanOrEqual(15);
      expect(control.resize_ack.oldest_age_ms).toBeGreaterThanOrEqual(35);
    } finally {
      if (priorInputUsage) keeper._pendingInputUsage.set(CHANNEL_ID, priorInputUsage);
      else keeper._pendingInputUsage.delete(CHANNEL_ID);
      if (priorResize) keeper.pendingResizes.set(resizeKey, priorResize);
      else keeper.pendingResizes.delete(resizeKey);
      manager.sessions.delete(CHANNEL_ID);
      manager.dispose();
    }
  });
});
