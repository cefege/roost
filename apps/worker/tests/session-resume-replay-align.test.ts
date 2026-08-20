// The worker/keeper-restart RESUME replay, as a parser-alignment problem.
//
// resume() feeds the keeper's retained history straight into a brand-new
// @wterm/core. The keeper's window opens at whatever byte its own fixed ring
// last evicted over (keeper/keeper-frame-handler.ts orderedHistory() reads
// outRing from offset 0), so under eviction the first replayed byte can be the
// tail of a sequence whose `ESC [` is gone. A cold parser prints that remnant as
// literal text and it never washes out: nothing downstream re-parses, and a
// TUI's cursor-addressed partial repaint never revisits a cell it believes it
// already painted. Production symptom: `32m1969M` burned into an htop grid.
//
// Only the pool's history/probe surface is stubbed — the replay loop, the ring
// seeding and the real WASM core are the production code paths.

import { describe, test, expect, afterEach } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared/wire";
import { gridToCellFrame } from "@roost/shared/cell";
import { ringLength } from "../src/session-scrollback-ring.ts";
import { KeeperFeature } from "../src/keeper/protocol.ts";
import { getMultiplexedPool } from "../src/keeper/multiplexed-client.ts";
import type { KeeperHistoryRecords } from "../src/keeper/multiplexed-client.ts";
import { installFakeKeeper, type FakeKeeper } from "./keeper-fake-pool.ts";
import type { ShellSpec } from "../src/shell-spec.ts";

const SESSION_ID = asSessionId("8c1e5b20-3333-4444-8555-666677778888");
const CHANNEL_ID = 11;
const CHILD_PID = 44221;

const SHELL_SPEC: ShellSpec = {
  version: 1,
  platform: "linux",
  executable: "/bin/sh",
  argv: [],
  cwd: "/",
  env: {},
};

/** The tail of `ESC [ 1 ; 32 m` left behind after eviction overwrote the lead. */
const ORPHAN_TAIL = "32m1969M";
/** What the TUI paints next: a cursor address, so it survives the prefix drop. */
const REPAINT = "\x1b[2;1HMEM-OK-REPAINT";

const enc = new TextEncoder();

interface Fixture {
  mgr: SessionManager;
  keeper: FakeKeeper;
  dispose(): void;
}

let live: Fixture | null = null;

afterEach(() => {
  live?.dispose();
  live = null;
});

/** Drive resume() against a keeper that reports `records` with `headSeq`.
 *  `headSeq > sum(record bytes)` is exactly how eviction is provable: head_seq
 *  counts every byte the pty ever produced. */
async function resumeWith(opts: {
  records: KeeperHistoryRecords["records"];
  headSeq: number;
}): Promise<Fixture> {
  // Before the manager: its constructor dials the pool, and a real keeper
  // adopted by that dial would replace the fake socket mid-test.
  const keeper = installFakeKeeper({ features: [KeeperFeature.OrderedHistory] });
  const pool = getMultiplexedPool();
  const priorListChannels = pool.listChannels;
  const priorReattach = pool.reattach;
  const priorHistoryRecords = pool.getHistoryRecords;
  pool.listChannels = async () => [{ channelId: CHANNEL_ID, pid: CHILD_PID }];
  pool.reattach = () => {};
  pool.getHistoryRecords = async () => ({
    headSeq: opts.headSeq,
    baseCols: 80,
    baseRows: 24,
    records: opts.records,
  });

  const mgr = new SessionManager({
    workerFp: asWorkerFp("11".repeat(32)),
    sink: { emit: () => {} },
    sendBinaryUpstream: () => "sent",
    sendCellGridUpstream: () => "sent",
  });
  // Git/port probes spawn subprocesses and say nothing about byte replay.
  mgr._startGitBranch = () => {};
  mgr._startPorts = () => {};

  const resumed = await mgr.resume({
    sessionId: SESSION_ID,
    channelId: asChannelId(CHANNEL_ID),
    kind: "shell",
    cwd: "/",
    shellSpec: SHELL_SPEC,
  });
  expect(resumed).toBe(true);

  const fixture: Fixture = {
    mgr,
    keeper,
    dispose() {
      pool.listChannels = priorListChannels;
      pool.reattach = priorReattach;
      pool.getHistoryRecords = priorHistoryRecords;
      keeper.restore();
    },
  };
  live = fixture;
  return fixture;
}

function coreText(mgr: SessionManager): string {
  const core = mgr.sessions.get(CHANNEL_ID)!.wtermCore;
  const frame = gridToCellFrame(core, 0, "resume-align:0");
  const lines: string[] = [];
  for (const row of frame.scrollbackRows) lines.push(row.spans.map((s) => s.text).join(""));
  for (const row of frame.viewportRows) lines.push(row.spans.map((s) => s.text).join(""));
  return lines.join("\n");
}

describe("resume replay into a cold core", () => {
  test("an evicted window opening mid-ESC[1;32m replays no literal text and keeps the repaint", async () => {
    const bytes = enc.encode(ORPHAN_TAIL + REPAINT);
    const f = await resumeWith({
      records: [{ kind: "output", bytes }],
      // The keeper produced 4 KiB more than it retained: the window's first byte
      // is an eviction cut, not the start of the stream.
      headSeq: bytes.byteLength + 4096,
    });

    const text = coreText(f.mgr);
    expect(text).not.toContain("32m");
    expect(text).not.toContain("1969M");
    expect(text).toContain("MEM-OK-REPAINT");
  });

  test("the ring keeps the untrimmed bytes so head_seq - ringLength stays the retained tail", async () => {
    const bytes = enc.encode(ORPHAN_TAIL + REPAINT);
    const headSeq = bytes.byteLength + 4096;
    const f = await resumeWith({ records: [{ kind: "output", bytes }], headSeq });

    // session-resize-capture derives retainedStart from this difference, so
    // dropping bytes from the ring would skew every later boundary offset.
    const rec = f.mgr.sessions.get(CHANNEL_ID)!;
    expect(ringLength(rec.scrollback)).toBe(bytes.byteLength);
    expect(rec.head_seq - ringLength(rec.scrollback)).toBe(4096);
  });

  test("an unevicted window is token-aligned by construction, so leading plain text survives", async () => {
    const bytes = enc.encode("hello-unevicted\r\n");
    const f = await resumeWith({
      records: [{ kind: "output", bytes }],
      // head_seq === retained: the window still starts at the true start of the
      // stream, so there is no orphan and nothing may be dropped.
      headSeq: bytes.byteLength,
    });

    expect(coreText(f.mgr)).toContain("hello-unevicted");
  });

  test("only the cold first write is trimmed; later records replay verbatim", async () => {
    const first = enc.encode(ORPHAN_TAIL + "\x1b[2;1HFIRST");
    // A bare continuation with no ESC at all: trimming this one would delete it.
    const second = enc.encode("-SECOND-VERBATIM");
    const f = await resumeWith({
      records: [
        { kind: "output", bytes: first },
        { kind: "output", bytes: second },
      ],
      headSeq: first.byteLength + second.byteLength + 4096,
    });

    const text = coreText(f.mgr);
    expect(text).not.toContain("32m");
    expect(text).toContain("FIRST-SECOND-VERBATIM");
  });
});
