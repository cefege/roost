// OSC 8 hyperlinks, worker side. Two independent contracts:
//
// 1. wterm-serialize.ts round-trips link identity the same way it round-trips
//    style. It already emitted SGR deltas; a grid replayed through it used to
//    come back with every hyperlink stripped, so a serialize-based fidelity
//    check was blind to link loss.
// 2. The core's OSC 8 table has a FIXED capacity. Once saturated the terminal
//    still paints perfectly and every NEW distinct link silently degrades to
//    plain text — no error, no missing output. The worker emits ONE Tier-1
//    signal per false→true flip so that state is not invisible.

import { describe, test, expect, afterEach, setSystemTime } from "bun:test";
import { WasmBridge } from "@wterm/core";
import { setSignalSink } from "@roost/shared/diag";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared/wire";
import { initCellEmitState } from "@roost/shared/cell";
import { SessionManager } from "../src/session-manager.ts";
import { serializeWTerm } from "../src/wterm-serialize.ts";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";
import { LifecycleTestSink } from "./lifecycle-test-sink.ts";

const enc = new TextEncoder();

/** Replay a grid through the serializer into a fresh core, as a resize rebuild
 *  or a scrollback capture would. */
async function replay(payload: string, cols = 40, rows = 6) {
  const server = await WasmBridge.load();
  server.init(cols, rows);
  server.writeRaw(enc.encode(payload));
  const client = await WasmBridge.load();
  client.init(cols, rows);
  client.writeRaw(enc.encode(serializeWTerm(server)));
  return { server, client };
}

/** Every cell of row 0 as [char, uri]. */
function rowLinks(core: { getCell(r: number, c: number): { char: number; linkUri?: string } }, cols: number) {
  const out: Array<[string, string | undefined]> = [];
  for (let col = 0; col < cols; col++) {
    const cell = core.getCell(0, col);
    if (cell.char === 0 || cell.char === 0x20) continue;
    out.push([String.fromCodePoint(cell.char), cell.linkUri]);
  }
  return out;
}

describe("wterm-serialize OSC 8 round-trip", () => {
  test("a replayed grid keeps each cell's exact URI", async () => {
    const { server, client } = await replay(
      "\x1b]8;;https://example.test/a\x1b\\link\x1b]8;;\x1b\\ plain\r\n",
    );
    const expected: Array<[string, string | undefined]> = [
      ["l", "https://example.test/a"], ["i", "https://example.test/a"],
      ["n", "https://example.test/a"], ["k", "https://example.test/a"],
      ["p", undefined], ["l", undefined], ["a", undefined], ["i", undefined], ["n", undefined],
    ];
    expect(rowLinks(server, 40)).toEqual(expected);
    expect(rowLinks(client, 40)).toEqual(expected);
  });

  test("two adjacent links sharing one URI stay two distinct links after replay", async () => {
    // The serializer must emit a re-open between them. Collapsing them would
    // fuse two separately clickable regions into one on every rebuild.
    const { client } = await replay(
      "\x1b]8;;https://example.test/same\x1b\\ab\x1b]8;;\x1b\\"
      + "\x1b]8;;https://example.test/same\x1b\\cd\x1b]8;;\x1b\\\r\n",
    );
    const keys = [0, 1, 2, 3].map((c) => client.getCell(0, c).linkKey);
    expect(keys[0]).toBe(keys[1]!);
    expect(keys[2]).toBe(keys[3]!);
    expect(keys[0]).not.toBe(keys[2]!);
    expect(client.getCell(0, 3).linkUri).toBe("https://example.test/same");
  });

  test("an explicit id= survives replay verbatim", async () => {
    const { client } = await replay("\x1b]8;id=tag1;https://example.test/y\x1b\\Y\x1b]8;;\x1b\\\r\n");
    expect(client.getCell(0, 0).linkUri).toBe("https://example.test/y");
    expect(client.getCell(0, 0).linkId).toBe("tag1");
  });

  test("a link in retained scrollback survives replay", async () => {
    // Eight lines through a 6-row grid pushes the linked line into history.
    const { client } = await replay(
      "\x1b]8;;https://example.test/old\x1b\\older\x1b]8;;\x1b\\\r\n"
      + "1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n",
    );
    const off = client.getScrollbackCount() - 1;
    expect(client.getScrollbackCell(off, 0).linkUri).toBe("https://example.test/old");
  });

  test("no open link leaks past the end of a replayed grid", async () => {
    // The last painted cell is inside a link that the producer never closed.
    const { client } = await replay("\x1b]8;;https://example.test/open\x1b\\tail");
    client.writeRaw(enc.encode("\r\nafter"));
    expect(client.getCell(1, 0).linkUri).toBeUndefined();
  });
});

// ── saturation signal ──────────────────────────────────────────────────────

const SID = "00000000-0000-4000-8000-00000000f00d";

/** A live session whose core's reported hyperlink resource state is ours to
 *  drive. Everything else is the real core and the real emit path. */
async function saturationHarness() {
  const signals: Array<Record<string, unknown>> = [];
  setSignalSink((record) => signals.push(record));
  const mgr = new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: new LifecycleTestSink(),
    sendBinaryUpstream: () => "sent",
    sendCellGridUpstream: () => "sent",
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
  const baseline = Promise.withResolvers<boolean>();
  mgr.terminalStreams.set(asChannelId(1), {
    streamId: "00000000-0000-4000-8000-000000000001",
    enabled: true,
    cols: 40,
    rows: 6,
    version: 1,
    baselineReady: true,
    coreValid: true,
    baselineDirty: false,
    snapshotCursor: null,
    resizeCapture: null,
    baselineInstalled: baseline.promise,
    baselinePromisePending: false,
    resolveBaselineInstalled: baseline.resolve,
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
