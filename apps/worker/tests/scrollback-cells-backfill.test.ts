// Viewport-only authoritative frames plus epoch-addressed, no-overlap history pages.

import { describe, expect, test } from "bun:test";
import { WasmBridge } from "@wterm/core";
import { asChannelId, asSessionId, asWorkerFp } from "@roost/protocol/wire";
import {
  gridToCellFrame,
  initCellEmitState,
  LIVE_DELTA_SCROLLBACK_ROWS_CAP,
  nextCellFrame,
  type CellRow,
} from "@roost/protocol/cell";
import type { ClientControlFrame } from "@roost/protocol/wire";
import { handleGetScrollbackCells } from "../src/browser-command-terminal.ts";
import type { FsmChannel } from "../src/fsm.ts";
import { SessionManager } from "../src/session-manager.ts";
import type { SessionShellRecord } from "../src/session-record.ts";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";
import type { CoordLink } from "../src/transport/coord-link.ts";
import { keeperTestShellSpec } from "./keeper-test-fixtures.ts";
import { SessionEventTestSink } from "./session-event-test-sink.ts";

const SID = asSessionId("00000000-0000-0000-0000-000000000001");
const CID = 1;
const COLS = 80;
const ROWS = 24;
const GRID_EPOCH = "test-grid:0";
const SEED = new TextEncoder().encode(
  Array.from({ length: 700 }, (_, index) => `line-${index}`).join("\r\n") + "\r\n",
);

function freshManager(): SessionManager {
	return new SessionManager({
		workerFp: asWorkerFp("00".repeat(32)),
		sink: new SessionEventTestSink(),
	});
}

async function injectSession(manager: SessionManager): Promise<SessionShellRecord> {
  const wtermCore = await WasmBridge.load();
  wtermCore.init(COLS, ROWS);
  wtermCore.writeRaw(SEED);
  const record: SessionShellRecord = {
    sessionId: SID,
    channelId: asChannelId(CID),
    socketPath: "/dev/null",
    kind: "shell",
    cwd: "/",
    shellSpec: keeperTestShellSpec({ executable: process.execPath, cwd: "/" }),
    fsm: {} as unknown as FsmChannel,
    scrollback: createSbRing(SEED),
    head_seq: SEED.length,
    alt_mode: false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    query_carry: new Uint8Array(0),
    ...initAgentOscState(),
    wtermCore,
    session_trace_id: "sbcell00",
    cell_emit: initCellEmitState("test-grid", "00000000-0000-4000-8000-000000000001"),
    lastPtyOutMs: 0,
    sb_origin_pin: null,
    spawnedAtMs: Date.now(),
    closeReservation: manager.reserveSessionEvent("closed"),
  };
  manager.sessions.set(CID, record);
  return record;
}

type RpcOk = {
  kind: "rpc-ok";
  request_id: string;
  data: {
    rows: CellRow[];
    cols: number;
    total: number;
    start_row: number;
    end_row: number;
    grid_epoch: string;
  };
};
type RpcError = { kind: "rpc-error"; request_id: string; message: string };

function linkCapture(): { coordLink: CoordLink; sent: Array<RpcOk | RpcError> } {
  const sent: Array<RpcOk | RpcError> = [];
  const coordLink = { send: (frame: RpcOk | RpcError) => { sent.push(frame); } } as unknown as CoordLink;
  return { coordLink, sent };
}

function request(
  endRow: number,
  maxRows: number,
  gridEpoch = GRID_EPOCH,
): Extract<ClientControlFrame, { kind: "get-scrollback-cells" }> {
  return {
    kind: "get-scrollback-cells",
    request_id: "req",
    session_id: SID,
    end_row: endRow,
    max_rows: maxRows,
    grid_epoch: gridEpoch,
  };
}

function rowText(row: CellRow): string {
  return row.spans.map((span) => span.text).join("");
}

describe("viewport-only frame and epoch-addressed history", () => {

  test("disjoint pages reconstruct the complete real-core history", async () => {
    const manager = freshManager();
    const record = await injectSession(manager);
    const { coordLink, sent } = linkCapture();
    const reference = gridToCellFrame(record.wtermCore, 1, GRID_EPOCH, "00000000-0000-4000-8000-000000000001");
    const collected: CellRow[] = [];
    let endRow = reference.scrollbackTotal;

    while (endRow > 0) {
      sent.length = 0;
      await handleGetScrollbackCells(
        request(endRow, 100),
        "req",
        { coordLink, sessionMgr: manager },
      );
      const reply = sent[0] as RpcOk;
      expect(reply.kind).toBe("rpc-ok");
      expect(reply.data.grid_epoch).toBe(GRID_EPOCH);
      expect(reply.data.end_row).toBe(endRow);
      collected.unshift(...reply.data.rows);
      endRow = reply.data.start_row;
    }

    expect(collected).toHaveLength(reference.scrollbackTotal);
    for (let index = 0; index < collected.length; index++) {
      expect(collected[index]!.index).toBe(index);
      expect(rowText(collected[index]!)).toBe(rowText(reference.scrollbackRows[index]!));
    }
  });

  test("a capped live checkpoint leaves retained rows demand-pageable", async () => {
    const manager = freshManager();
    const record = await injectSession(manager);
    const initial = nextCellFrame(record.wtermCore, record.cell_emit, false, 0);
    record.cell_emit = initial.state;
    record.wtermCore.clearDirty();

    const appended = Array.from(
      { length: LIVE_DELTA_SCROLLBACK_ROWS_CAP + 1 },
      (_, index) => `checkpoint-${index}`,
    ).join("\r\n") + "\r\n";
    record.wtermCore.writeRaw(new TextEncoder().encode(appended));
    const checkpoint = nextCellFrame(record.wtermCore, record.cell_emit, false, 0);
    record.cell_emit = checkpoint.state;
    expect(checkpoint.frame).toMatchObject({
      full: true,
      gridEpoch: GRID_EPOCH,
      seq: 2,
      baseSeq: 0,
      scrollbackRows: [],
      scrollbackAppend: [],
    });
    expect(checkpoint.frame.scrollbackTotal - initial.frame.scrollbackTotal)
      .toBeGreaterThan(LIVE_DELTA_SCROLLBACK_ROWS_CAP);

    const { coordLink, sent } = linkCapture();
    await handleGetScrollbackCells(
      request(checkpoint.frame.scrollbackTotal, 1, checkpoint.frame.gridEpoch),
      "req",
      { coordLink, sessionMgr: manager },
    );

    const reply = sent[0] as RpcOk;
    expect(reply.kind).toBe("rpc-ok");
    expect(reply.data.rows).toHaveLength(1);
    expect(reply.data.rows[0]!.index).toBe(checkpoint.frame.scrollbackTotal - 1);
    expect(rowText(reply.data.rows[0]!)).toContain("checkpoint-");
  });

  test("an empty headless epoch binds the read to the current grid", async () => {
    const manager = freshManager();
    const record = await injectSession(manager);
    const { coordLink, sent } = linkCapture();

    await handleGetScrollbackCells(
      request(record.wtermCore.getScrollbackCount(), 100, ""),
      "req",
      { coordLink, sessionMgr: manager },
    );

    const reply = sent[0] as RpcOk;
    expect(reply.kind).toBe("rpc-ok");
    expect(reply.data.grid_epoch).toBe(GRID_EPOCH);
    expect(reply.data.rows.length).toBeGreaterThan(0);
  });

  test("an epoch change during the 250-row yield aborts the page", async () => {
    const manager = freshManager();
    const record = await injectSession(manager);
    const { coordLink, sent } = linkCapture();
    const changed = Promise.withResolvers<void>();
    setImmediate(() => {
      record.cell_emit.gridEpochRevision += 1;
      changed.resolve();
    });

    await handleGetScrollbackCells(
      request(record.wtermCore.getScrollbackCount(), 600),
      "req",
      { coordLink, sessionMgr: manager },
    );
    await changed.promise;

    expect(sent).toEqual([
      { kind: "rpc-error", request_id: "req", message: "grid epoch changed" },
    ]);
  });
});
