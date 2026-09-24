// The coordinator's read model for owner-mode sessions: the capability
// negotiated at hello, the session ownership that survives the route-cache
// sweep that same hello performs, and the membership a worker publishes
// through WTerminalViewProjection answering the four terminal-view accessors
// diagnostics, presence seeding and push suppression read.

import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  WHelloSchema,
  WTerminalViewProjectionSchema,
  type CoordWorkerDown,
} from "@roost/protocol/proto/worker_transport_pb";
import { PbTerminalViewInputSchema } from "@roost/protocol/proto/wire_pb";
import {
  activeTerminalViewerFingerprints,
  applyWorkerTerminalViewProjection,
  installTerminalViewHub,
  terminalViewInputs,
  terminalViewSnapshot,
} from "../src/connect/terminal-view-hub.ts";
import {
  TERMINAL_VIEW_OWNER_CAPABILITY,
  registerTerminalViewOwner,
  terminalViewOwnerForSession,
} from "../src/connect/terminal-view-projection.ts";
import { makeWorkerConn, type WorkerServiceDeps } from "../src/connect/worker-conn.ts";
import { cacheSessionWorker, evictSessionWorker } from "../src/byte-hub.ts";
import {
  OWNER_FP,
  STREAM_B,
  makeOwnerHarness,
  resetOwnerMode,
} from "./terminal-view-owner-harness.ts";
import { SESSION, VIEW_A, VIEW_B } from "./terminal-view-hub-harness.ts";

interface PublishedViewer {
  fingerprint: string;
  viewId: string;
  cols: number;
  rows: number;
  parked?: boolean;
  constrains?: boolean;
}

function publish(viewers: PublishedViewer[]): void {
  applyWorkerTerminalViewProjection(OWNER_FP, create(WTerminalViewProjectionSchema, {
    sessionId: SESSION,
    viewers: viewers.map((viewer) => create(PbTerminalViewInputSchema, {
      fingerprint: viewer.fingerprint,
      viewId: viewer.viewId,
      cols: viewer.cols,
      rows: viewer.rows,
      parked: viewer.parked ?? false,
      constrains: viewer.constrains ?? true,
    })),
    effectiveCols: 80,
    effectiveRows: 24,
    streamId: STREAM_B,
  }));
}

function helloFrame(capabilities: string[]) {
  return create(CoordWorkerUpSchema, {
    frame: {
      case: "hello",
      value: create(WHelloSchema, { workerFp: OWNER_FP, version: "test", capabilities }),
    },
  });
}

function connectWorker(sent: CoordWorkerDown[]) {
  return makeWorkerConn(
    {} as WorkerServiceDeps,
    { fingerprint: OWNER_FP },
    (frame) => { sent.push(frame); return 1; },
    () => undefined,
  );
}

function ackCapabilities(sent: CoordWorkerDown[]): readonly string[] | null {
  const ack = sent.find((frame) => frame.frame.case === "helloAck");
  return ack?.frame.case === "helloAck" ? ack.frame.value.capabilities : null;
}

afterEach(resetOwnerMode);

describe("owner-mode membership projection", () => {
  test("the worker's membership answers the coordinator accessors and clears on disconnect", () => {
    const { hub } = makeOwnerHarness();
    installTerminalViewHub(hub);
    const registration = registerTerminalViewOwner(OWNER_FP);

    publish([
      { fingerprint: "device-1", viewId: VIEW_A, cols: 100, rows: 30 },
      { fingerprint: "device-2", viewId: VIEW_B, cols: 80, rows: 24, parked: true, constrains: false },
    ]);

    expect([...activeTerminalViewerFingerprints(SESSION)].sort()).toEqual(["device-1", "device-2"]);
    expect(terminalViewInputs(SESSION)).toEqual([
      { fingerprint: "device-1", viewId: VIEW_A, cols: 100, rows: 30, parked: false, constrains: true },
      { fingerprint: "device-2", viewId: VIEW_B, cols: 80, rows: 24, parked: true, constrains: false },
    ]);
    expect(terminalViewSnapshot(SESSION)).toEqual({
      activeViews: 1,
      parkedViews: 1,
      streamId: STREAM_B,
      effective: { cols: 80, rows: 24 },
      unavailable: false,
    });

    registration.release();

    expect([...activeTerminalViewerFingerprints(SESSION)]).toEqual([]);
    expect(terminalViewInputs(SESSION)).toEqual([]);
    expect(terminalViewSnapshot(SESSION)).toBeNull();
  });

  // Legacy keeps reporting a session whose views all went inactive until the
  // session closes; owner mode must not make that row vanish instead.
  test("an empty viewer list leaves a zero-viewer row until the session closes", () => {
    const { hub } = makeOwnerHarness();
    installTerminalViewHub(hub);
    registerTerminalViewOwner(OWNER_FP);

    publish([{ fingerprint: "device-1", viewId: VIEW_A, cols: 100, rows: 30 }]);
    expect(terminalViewInputs(SESSION)).toHaveLength(1);

    publish([]);
    expect(terminalViewInputs(SESSION)).toEqual([]);
    expect(terminalViewSnapshot(SESSION)).toMatchObject({ activeViews: 0, parkedViews: 0 });

    hub.closeSession(SESSION);
    expect(terminalViewSnapshot(SESSION)).toBeNull();
  });

  test("a worker that never advertised ownership cannot publish membership", () => {
    const { hub } = makeOwnerHarness();
    installTerminalViewHub(hub);

    publish([{ fingerprint: "device-1", viewId: VIEW_A, cols: 100, rows: 30 }]);

    expect(terminalViewInputs(SESSION)).toEqual([]);
  });
});

describe("worker hello capability negotiation", () => {
  test("an advertised owner capability is echoed and claims the fingerprint's sessions", async () => {
    const sent: CoordWorkerDown[] = [];
    const conn = connectWorker(sent);
    try {
      await conn.handleUpstream(helloFrame([TERMINAL_VIEW_OWNER_CAPABILITY]));

      expect(ackCapabilities(sent)).toEqual([TERMINAL_VIEW_OWNER_CAPABILITY]);

      cacheSessionWorker(SESSION, OWNER_FP, 7);
      expect(terminalViewOwnerForSession(SESSION)).toBe(OWNER_FP);

      // A worker hello sweeps its own route-cache entries; a view heartbeat
      // landing in that window must still route to the owner, or the
      // coordinator would admit a second minimizer for the same session.
      evictSessionWorker(SESSION);
      expect(terminalViewOwnerForSession(SESSION)).toBe(OWNER_FP);
    } finally {
      conn.close();
    }
    expect(terminalViewOwnerForSession(SESSION)).toBeNull();
  });

  test("a legacy worker is neither echoed nor made an owner", async () => {
    const sent: CoordWorkerDown[] = [];
    const conn = connectWorker(sent);
    try {
      await conn.handleUpstream(helloFrame([]));

      expect(ackCapabilities(sent)).toEqual([]);
      cacheSessionWorker(SESSION, OWNER_FP, 7);
      expect(terminalViewOwnerForSession(SESSION)).toBeNull();
    } finally {
      conn.close();
    }
  });

  // The superseded connection's identity-stamped release only runs when its
  // socket close event lands, which is strictly after the replacement hello.
  // The hello therefore has to settle ownership in both directions, or a
  // downgraded worker keeps being sent relays it silently drops.
  test("a worker that reconnects without the capability stops owning its sessions", async () => {
    const owner = connectWorker([]);
    await owner.handleUpstream(helloFrame([TERMINAL_VIEW_OWNER_CAPABILITY]));
    cacheSessionWorker(SESSION, OWNER_FP, 7);
    expect(terminalViewOwnerForSession(SESSION)).toBe(OWNER_FP);

    const downgraded = connectWorker([]);
    try {
      await downgraded.handleUpstream(helloFrame([]));
      cacheSessionWorker(SESSION, OWNER_FP, 7);

      expect(terminalViewOwnerForSession(SESSION)).toBeNull();
      owner.close();
      expect(terminalViewOwnerForSession(SESSION)).toBeNull();
    } finally {
      downgraded.close();
      owner.close();
    }
  });
});
