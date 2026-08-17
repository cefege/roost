// Viewport-only authoritative frames plus epoch-addressed, no-overlap history pages.

import { describe, expect, test } from "bun:test";
import { WasmBridge } from "@wterm/core";
import { asChannelId, asSessionId, asWorkerFp } from "@roost/shared";
import {
  gridToCellFrame,
  initCellEmitState,
  SB_SNAPSHOT_HISTORY_ROWS,
  type CellRow,
} from "@roost/shared/cell";
import { protoToCellFrame } from "@roost/shared/cell/cell-proto";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import type { ClientControlFrame } from "@roost/shared/wire";
import { handleGetScrollbackCells } from "../src/browser-command-terminal.ts";
import type { FsmChannel } from "../src/fsm.ts";
import { SessionManager } from "../src/session-manager.ts";
import type { SessionShellRecord } from "../src/session-record.ts";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";
import type { CoordLink } from "../src/transport/CoordLink.ts";
import { keeperTestShellSpec } from "./keeper-test-fixtures.ts";

const SID = asSessionId("00000000-0000-0000-0000-000000000001");
const CID = 1;
const COLS = 80;
const ROWS = 24;
const GRID_EPOCH = "test-grid:0";
const SEED = new TextEncoder().encode(
  Array.from({ length: 700 }, (_, index) => `line-${index}`).join("\r\n") + "\r\n",
);

function freshManager(onCellFrame?: (frame: PbCellGridFrame) => void): SessionManager {
  return new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
    ...(onCellFrame ? { sendCellGridUpstream: (_channelId, frame) => onCellFrame(frame) } : {}),
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
    ...initAgentOscState(),
    wtermCore,
    session_trace_id: "sbcell00",
    cell_emit: initCellEmitState("test-grid"),
    lastPtyOutMs: 0,
    spawnedAtMs: Date.now(),
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
  test("a forced authoritative frame carries no historical rows", async () => {
    const frames: PbCellGridFrame[] = [];
    const manager = freshManager((frame) => frames.push(frame));
    const record = await injectSession(manager);
    const total = record.wtermCore.getScrollbackCount();
    expect(total).toBeGreaterThan(0);

    manager.emitCellSnapshot(asChannelId(CID));
    const frame = protoToCellFrame(frames[0]!);
    expect(frame.full).toBe(true);
    expect(frame.gridEpoch).toBe(GRID_EPOCH);
    expect(frame.scrollbackRows).toHaveLength(SB_SNAPSHOT_HISTORY_ROWS);
    expect(frame.scrollbackTotal).toBe(total);
    expect(frame.sbBase).toBe(frame.scrollbackTotal);
  });

  test("disjoint pages reconstruct the complete real-core history", async () => {
    const manager = freshManager();
    const record = await injectSession(manager);
    const { coordLink, sent } = linkCapture();
    const reference = gridToCellFrame(record.wtermCore, 1, GRID_EPOCH);
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
