// The SCD is only debuggable in production if the diagnostic reports the
// inputs it minimized over, not just the result. These tests pin that the
// per-session diagnostic enumerates every watching view, flags which ones
// still constrain geometry, and that the reported effective size IS the
// minimum of exactly those flagged inputs.

import { afterEach, expect, test } from "bun:test";
import {
  TERMINAL_VIEW_PARK_GRACE_MS,
  minimumTerminalGeometry,
} from "@roost/shared/viewport";
import { coordSessionDiagnostic } from "../src/connect/diag-snapshot-session-state.ts";
import { installTerminalViewHub } from "../src/connect/terminal-view-hub.ts";
import {
  SESSION,
  VIEW_A,
  VIEW_B,
  WORKER,
  disposeHubs,
  makeHarness,
  register,
  settle,
  sweep,
  viewCommand,
} from "./terminal-view-hub-harness.ts";

const ADMITTED = {
  allowedWorkerFps: new Set([WORKER]),
  dispatchableWorkerFps: new Set([WORKER]),
};

afterEach(() => {
  installTerminalViewHub(null);
  disposeHubs();
});

function diagnose() {
  return coordSessionDiagnostic(
    { id: SESSION, worker_fp: WORKER, channel: 7 },
    ADMITTED,
  );
}

test("reports every viewer input and minimizes over the constraining ones", async () => {
  const { hub, clock } = makeHarness();
  installTerminalViewHub(hub);
  register(hub, "socket-a", "viewer-a", "fingerprint-a");
  register(hub, "socket-b", "viewer-b", "fingerprint-b");
  hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 100, rows: 30 }));
  hub.handleViewCommand("socket-b", viewCommand(VIEW_B, 1n, { cols: 90, rows: 40 }));
  await settle();

  const live = diagnose();
  expect(live.route).toEqual({
    worker_fp: WORKER,
    channel_id: 7,
    connected: true,
    source: "database",
  });
  expect([...live.viewers].sort((left, right) => left.viewId.localeCompare(right.viewId)))
    .toEqual([
      {
        fingerprint: "fingerprint-a",
        viewId: VIEW_A,
        cols: 100,
        rows: 30,
        parked: false,
        constrains: true,
      },
      {
        fingerprint: "fingerprint-b",
        viewId: VIEW_B,
        cols: 90,
        rows: 40,
        parked: false,
        constrains: true,
      },
    ]);
  expect(live.terminal_view?.effective).toEqual({ cols: 90, rows: 30 });
  expect(live.terminal_view?.effective).toEqual(
    minimumTerminalGeometry(live.viewers.filter((input) => input.constrains)),
  );

  // A dead socket keeps its membership for reclaim but stops constraining once
  // the park grace lapses, so the session re-minimizes over the survivor.
  hub.closeSocket("socket-b");
  clock.value += TERMINAL_VIEW_PARK_GRACE_MS + 1;
  sweep(hub);
  await settle();

  const parked = diagnose();
  expect([...parked.viewers].sort((left, right) => left.viewId.localeCompare(right.viewId)))
    .toMatchObject([
      { viewId: VIEW_A, parked: false, constrains: true },
      { viewId: VIEW_B, cols: 90, rows: 40, parked: true, constrains: false },
    ]);
  expect(parked.terminal_view?.effective).toEqual({ cols: 100, rows: 30 });
  expect(parked.terminal_view?.effective).toEqual(
    minimumTerminalGeometry(parked.viewers.filter((input) => input.constrains)),
  );
});

test("withholds the route for a worker the caller was not admitted to", async () => {
  const { hub } = makeHarness();
  installTerminalViewHub(hub);
  register(hub, "socket-a", "viewer-a", "fingerprint-a");
  hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 100, rows: 30 }));
  await settle();

  const denied = coordSessionDiagnostic(
    { id: SESSION, worker_fp: WORKER, channel: 7 },
    { allowedWorkerFps: new Set(), dispatchableWorkerFps: new Set() },
  );
  expect(denied.route).toBeNull();
});
