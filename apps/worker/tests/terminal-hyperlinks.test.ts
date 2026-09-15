// OSC 8 hyperlinks, worker side: the core's link table has a FIXED capacity, and
// once it saturates every NEW distinct link degrades to plain text — no error, no
// missing output. Pins the ONE Tier-1 signal per false→true flip, its re-arming,
// and the per-session diagnostic counts, against a real loaded core.

import { describe, test, expect, afterEach, setSystemTime } from "bun:test";
import { WasmBridge } from "@wterm/core";
import { setSignalSink } from "@roost/shared/diag";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared/wire";
import { initCellEmitState } from "@roost/shared/cell";
import { SessionManager } from "../src/session-manager.ts";
import { COORD_CELL_SINK_ID, registerCellSink } from "../src/session-cell-sinks.ts";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";
import { SessionEventTestSink } from "./session-event-test-sink.ts";

const SID = "00000000-0000-4000-8000-00000000f00d";

/** A live session whose core's reported hyperlink resource state is ours to
 *  drive. Everything else is the real core and the real emit path. */
async function saturationHarness() {
  const signals: Array<Record<string, unknown>> = [];
  setSignalSink((record) => signals.push(record));
  const mgr = new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: new SessionEventTestSink(),
    sendBinaryUpstream: () => "sent",
  });
  registerCellSink(mgr, {
    id: COORD_CELL_SINK_ID,
    sendFrame: () => "sent",
    sendChunk: () => "sent",
  });
  const wtermCore = await WasmBridge.load();
  wtermCore.init(40, 6);
  const links = { capacity: 512, used: 1, rejected: 0, saturated: false };
  wtermCore.getResourceState = () => ({ hyperlinks: links });
  (mgr as unknown as { sessions: Map<number, unknown> }).sessions.set(1, {
    sessionId: asSessionId(SID),
    channelId: asChannelId(1),
    socketPath: "/dev/null",
    kind: "shell" as const,
    cwd: "/",
    fsm: {} as never,
    bridge: null,
    scrollback: createSbRing(),
    head_seq: 0,
    alt_mode: false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    query_carry: new Uint8Array(0),
    ...initAgentOscState(),
    wtermCore,
    cell_emit: initCellEmitState("sat-grid", "00000000-0000-4000-8000-000000000001"),
    lastPtyOutMs: 0,
  });
  mgr.terminalStreams.set(asChannelId(1), {
    streamId: "00000000-0000-4000-8000-000000000001",
    enabled: true,
    cols: 40,
    rows: 6,
    version: 1,
    coreValid: true,
    deliveries: new Map([
      [COORD_CELL_SINK_ID, { cursor: null, baselineReady: true, baselineDirty: false }],
    ]),
    resizeCapture: null,
  });
  return { mgr, links, signals };
}

describe("hyperlink table saturation", () => {
  afterEach(() => {
    setSignalSink(null);
    setSystemTime();
  });

  test("fires exactly once per false→true flip and re-arms after the table clears", async () => {
    // The signal channel coalesces repeats of one kind+scope for 10 s, so each
    // deliberate flip is separated by a clock step rather than a real sleep.
    setSystemTime(new Date("2026-08-18T00:00:00Z"));
    const { mgr, links, signals } = await saturationHarness();

    mgr.emitCellFrame(1, true);
    expect(signals).toHaveLength(0);

    links.saturated = true;
    links.used = 512;
    links.rejected = 3;
    mgr.emitCellFrame(1, true);
    mgr.emitCellFrame(1, true);
    mgr.emitCellFrame(1, true);
    expect(signals).toHaveLength(1);
    expect(signals[0]).toMatchObject({
      evt: "terminal.hyperlink_saturated",
      sid: SID,
      channel_id: 1,
      capacity: 512,
      used: 512,
      rejected: 3,
    });

    // A core rebuild empties the table; the next frame must re-arm the edge.
    setSystemTime(new Date("2026-08-18T00:00:11Z"));
    links.saturated = false;
    mgr.emitCellFrame(1, true);
    expect(signals).toHaveLength(1);

    links.saturated = true;
    mgr.emitCellFrame(1, true);
    expect(signals).toHaveLength(2);
  });

  test("the per-session diagnostic snapshot carries the table counts", async () => {
    const { mgr, links } = await saturationHarness();
    links.used = 7;
    links.rejected = 2;
    links.saturated = true;
    // diagSnapshot's declared return is Record<string, unknown>; the per-session
    // shape it builds is only expressible here, at the assertion boundary.
    const sessions = mgr.diagSnapshot().sessions as Record<string, { terminal: { hyperlinks: unknown } }>;
    expect(sessions[SID]!.terminal.hyperlinks).toEqual({
      capacity: 512, used: 7, rejected: 2, saturated: true,
    });
  });
});
