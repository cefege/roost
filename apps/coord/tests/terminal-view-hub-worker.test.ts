import { afterEach, describe, expect, test, vi } from "bun:test";
import { TerminalViewStatus } from "@roost/shared/proto/sync_pb";
import {
  TerminalStreamFailureKind,
  TerminalStreamStatus,
} from "@roost/shared/proto/worker_transport_pb";
import {
  DASHBOARD,
  SESSION,
  VIEW_A,
  WORKER,
  type Route,
  admitted,
  deferred,
  disposeHubs,
  makeHarness,
  register,
  resultFor,
  settle,
  statesFor,
  viewCommand,
} from "./terminal-view-hub-harness.ts";
import { deltaFrame } from "./terminal-screen-hub-harness.ts";
import { TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS } from "../src/connect/terminal-screen-hub.ts";

afterEach(disposeHubs);

describe("TerminalViewHub worker transition ownership", () => {
  test("redrives matching worker replacement with a fresh stream generation", async () => {
    const { hub, sent } = makeHarness();
    register(hub);
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 90, rows: 30 }));
    await settle();
    expect(sent).toHaveLength(1);
    const firstStream = sent[0]!.streamId;

    hub.workerReplacement("another-worker");
    await settle();
    expect(sent).toHaveLength(1);

    hub.workerReplacement(WORKER);
    await settle();
    expect(sent).toHaveLength(2);
    expect(sent[1]).toMatchObject({ workerFp: WORKER, enabled: true, cols: 90, rows: 30 });
    expect(sent[1]!.streamId).not.toBe(firstStream);
    expect(hub.snapshot(SESSION)?.streamId).toBe(sent[1]!.streamId);
  });

  test("coalesces unresolved desired transitions to the newest stream", async () => {
    const route = deferred<Route>();
    const { hub, sent } = makeHarness({ resolveRoute: () => route.promise });
    const sink = register(hub);

    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 80, rows: 24 }));
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 2n, { cols: 90, rows: 30 }));
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 3n, { cols: 100, rows: 40 }));
    const desiredStreams = statesFor(sink, VIEW_A).map((state) => state.streamId);
    expect(new Set(desiredStreams).size).toBe(3);
    expect(sent).toHaveLength(0);

    route.resolve({ workerFp: WORKER, channel: 7, dashboardId: DASHBOARD });
    await settle();
    expect(sent).toHaveLength(1);
    expect(sent[0]).toMatchObject({ enabled: true, cols: 100, rows: 40 });
    expect(sent[0]!.streamId).toBe(desiredStreams.at(-1)!);
    expect(hub.snapshot(SESSION)?.streamId).toBe(sent[0]!.streamId);
  });

  test("uses a fresh stream ID for the single retryable pre-write retry", async () => {
    let attempts = 0;
    const { hub, sent } = makeHarness({
      sendStreamState: (_workerFp, state) => {
        attempts += 1;
        const result = attempts === 1
          ? resultFor(state, TerminalStreamStatus.REJECTED, TerminalStreamFailureKind.RETRYABLE_PRE_WRITE)
          : resultFor(state);
        return admitted(Promise.resolve(result));
      },
    });
    const sink = register(hub);

    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 88, rows: 33 }));
    await settle();

    expect(sent).toHaveLength(2);
    expect(sent.map(({ enabled, cols, rows }) => ({ enabled, cols, rows }))).toEqual([
      { enabled: true, cols: 88, rows: 33 },
      { enabled: true, cols: 88, rows: 33 },
    ]);
    expect(sent[1]!.streamId).not.toBe(sent[0]!.streamId);
    expect(statesFor(sink, VIEW_A).at(-1)?.streamId).toBe(sent[1]!.streamId);
    expect(hub.snapshot(SESSION)?.streamId).toBe(sent[1]!.streamId);
  });

  test("redrives an exhausted retryable failure only from an exact view heartbeat", async () => {
    let attempts = 0;
    const { hub, sent } = makeHarness({
      sendStreamState: (_workerFp, state) => {
        attempts += 1;
        const result = attempts <= 2
          ? resultFor(
            state,
            TerminalStreamStatus.REJECTED,
            TerminalStreamFailureKind.RETRYABLE_PRE_WRITE,
          )
          : resultFor(state);
        return admitted(Promise.resolve(result));
      },
    });
    const sink = register(hub);
    const command = viewCommand(VIEW_A, 1n, { cols: 95, rows: 35 });
    hub.handleViewCommand("socket-a", command);
    await settle();
    expect(sent).toHaveLength(2);
    expect(hub.snapshot(SESSION)?.unavailable).toBe(true);
    expect(statesFor(sink, VIEW_A).at(-1)?.status).toBe(TerminalViewStatus.UNAVAILABLE);

    hub.handleViewCommand("socket-a", command);
    await settle();
    expect(sent).toHaveLength(3);
    expect(new Set(sent.map((state) => state.streamId)).size).toBe(3);
    expect(hub.snapshot(SESSION)).toMatchObject({
      streamId: sent[2]!.streamId,
      unavailable: false,
    });
  });

  test("keeps fail-closed worker failures down until their route reconciles", async () => {
    for (const failureKind of [
      TerminalStreamFailureKind.SESSION_NOT_LIVE,
      TerminalStreamFailureKind.CORE_FAILED,
      TerminalStreamFailureKind.AMBIGUOUS_BOUNDARY,
    ]) {
      let attempts = 0;
      const { hub, sent } = makeHarness({
        sendStreamState: (_workerFp, state) => {
          attempts += 1;
          const result = attempts === 1
            ? resultFor(state, TerminalStreamStatus.REJECTED, failureKind)
            : resultFor(state);
          return admitted(Promise.resolve(result));
        },
      });
      const sink = register(
        hub,
        `route-socket-${failureKind}`,
        `route-viewer-${failureKind}`,
        `route-fingerprint-${failureKind}`,
      );
      const command = viewCommand(VIEW_A, 1n);
      hub.handleViewCommand(`route-socket-${failureKind}`, command);
      await settle();
      expect(sent).toHaveLength(1);
      expect(hub.snapshot(SESSION)?.unavailable).toBe(true);

      hub.handleViewCommand(`route-socket-${failureKind}`, command);
      await settle();
      expect(sent).toHaveLength(1);
      expect(statesFor(sink, VIEW_A).at(-1)?.status).toBe(TerminalViewStatus.UNAVAILABLE);

      hub.routeReconciled(WORKER, [SESSION]);
      await settle();
      expect(sent).toHaveLength(2);
      expect(sent[1]!.streamId).not.toBe(sent[0]!.streamId);
      expect(hub.snapshot(SESSION)?.unavailable).toBe(false);
    }
  });

  test("never redrives an invalid worker request from heartbeat or route events", async () => {
    const { hub, sent } = makeHarness({
      sendStreamState: (_workerFp, state) => admitted(Promise.resolve(
        resultFor(
          state,
          TerminalStreamStatus.REJECTED,
          TerminalStreamFailureKind.INVALID_REQUEST,
        ),
      )),
    });
    const sink = register(hub);
    const command = viewCommand(VIEW_A, 1n);
    hub.handleViewCommand("socket-a", command);
    await settle();
    expect(sent).toHaveLength(1);
    expect(hub.snapshot(SESSION)?.unavailable).toBe(true);

    hub.handleViewCommand("socket-a", command);
    hub.routeReconciled(WORKER, [SESSION]);
    hub.workerReplacement(WORKER);
    await settle();
    expect(sent).toHaveLength(1);
    expect(statesFor(sink, VIEW_A).at(-1)?.status).toBe(TerminalViewStatus.UNAVAILABLE);
  });

  test("no-first-byte repair mints a fresh stream and publishes redrive failure", async () => {
    vi.useFakeTimers();
    let attempts = 0;
    try {
      const { hub, sent, snapshotRequests } = makeHarness({
        sendStreamState: (_workerFp, state) => {
          attempts++;
          if (attempts === 1) {
            return admitted(Promise.resolve(resultFor(state)));
          }
          return {
            admitted: false,
            expired: false,
            requestId: null,
            result: Promise.reject(new Error("redrive transport unavailable")),
          };
        },
      });
      const sink = register(hub);
      hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n));
      await settle();
      const firstStream = sent[0]!.streamId;

      hub.screen.publishFrame(SESSION, deltaFrame({
        streamId: firstStream,
        cols: 80,
        rows: 24,
      }));
      await settle();
      expect(snapshotRequests).toHaveLength(1);

      vi.advanceTimersByTime(TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS);
      await settle();
      expect(snapshotRequests).toHaveLength(2);
      vi.advanceTimersByTime(TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS);
      await settle();

      expect(sent).toHaveLength(2);
      expect(sent[1]!.streamId).not.toBe(firstStream);
      expect(hub.snapshot(SESSION)?.streamId).toBe(sent[1]!.streamId);
      expect(statesFor(sink, VIEW_A).at(-1)?.status)
        .toBe(TerminalViewStatus.UNAVAILABLE);
    } finally {
      disposeHubs();
      vi.useRealTimers();
    }
  });
});
