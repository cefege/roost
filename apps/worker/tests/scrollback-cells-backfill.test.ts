// Lazy-history attach: tail full frames + get-scrollback-cells backfill.
//
// The attach snapshot (emitCellFrame force=true) carries only the newest
// SB_SNAPSHOT_TAIL_ROWS scrollback lines (sbBase = total - tail); the SPA
// pulls [0, sbBase) per-viewer via handleGetScrollbackCells. These tests pin
// the worker half of that contract against a REAL wterm core:
//
//   T1 — forced full frame is tail-capped: sb rows == SB_SNAPSHOT_TAIL_ROWS,
//        sbBase == total - tail, scrollbackTotal == full grid depth.
//   T2 — backfill reconstruction: walking handleGetScrollbackCells down from
//        sbBase and concatenating with the tail reproduces the UNTAILED
//        gridToCellFrame row-for-row (index + text) — no gap, no dup, no
//        reorder at any chunk seam.
//   T3 — end_row beyond the grid clamps; unknown session → rpc-error.
//   T4 — a fetch racing a scheduled rebuild (OPT2-1 chain) serves rows from
//        the POST-rebuild grid (new width), never the mid-swap old core.
//
// L11 class this guards: "attach/resize slow proportional to scrollback
// depth" (tail + backfill fix) without regressing the seam-torn class.

import { describe, test, expect } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { handleGetScrollbackCells } from "../src/browser-command-terminal.ts";
import type { CoordLink } from "../src/transport/CoordLink.ts";
import type { SessionRecord } from "../src/session-record.ts";
import type { FsmChannel } from "../src/fsm.ts";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared";
import type { ClientControlFrame } from "@roost/shared/wire";
import {
  gridToCellFrame, initCellEmitState, SB_SNAPSHOT_TAIL_ROWS,
  type CellRow,
} from "@roost/shared/cell";
import { protoToCellFrame } from "@roost/shared/cell/cell-proto";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import { WasmBridge } from "@wterm/core";

const SID = asSessionId("00000000-0000-0000-0000-000000000001");
const CID = 1;
const COLS = 80, ROWS = 24;
// 400 numbered lines at 80 cols → ~376 scrollback rows, comfortably > the
// 250-row tail so sbBase lands mid-history.
const SEED = new TextEncoder().encode(
  Array.from({ length: 400 }, (_, i) => `line-${i}`).join("\r\n") + "\r\n",
);

const rowTextOf = (r: CellRow): string => r.spans.map((s) => s.text).join("");

function freshMgr(onCellFrame?: (frame: PbCellGridFrame) => void): SessionManager {
  return new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
    hookSocketPath: "/dev/null",
    ...(onCellFrame ? { sendCellGridUpstream: (_cid, frame) => onCellFrame(frame) } : {}),
  });
}

async function injectSession(mgr: SessionManager, bytes: Uint8Array): Promise<SessionRecord> {
  const wtermCore = await WasmBridge.load();
  wtermCore.init(COLS, ROWS);
  wtermCore.writeRaw(bytes);
  // Test double: nothing on the tail/backfill path touches the FSM.
  const fsm = {} as unknown as FsmChannel;
  const record: SessionRecord = {
    sessionId: SID,
    channelId: asChannelId(CID),
    socketPath: "/dev/null",
    kind: "shell",
    cwd: "/",
    fsm,
    scrollback: new Uint8Array(bytes),
    head_seq: bytes.length,
    alt_mode: false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    wtermCore,
    session_trace_id: "sbcell00",
    cell_emit: initCellEmitState(),
    spawnedAtMs: Date.now(),
    chat_seq: 0,
  };
  mgr.sessions.set(CID, record);
  return record;
}

interface RpcOk {
  kind: "rpc-ok";
  request_id: string;
  data: { rows: CellRow[]; cols: number; total: number; start_row: number; end_row: number };
}
interface RpcErr { kind: "rpc-error"; request_id: string; message: string }

function makeLinkCapture(): { coordLink: CoordLink; sent: Array<RpcOk | RpcErr> } {
  const sent: Array<RpcOk | RpcErr> = [];
  const stub = { send: (f: RpcOk | RpcErr) => { sent.push(f); } };
  // Test double: handleGetScrollbackCells only calls coordLink.send.
  const coordLink = stub as unknown as CoordLink;
  return { coordLink, sent };
}

function cellsFrame(endRow: number, maxRows: number): Extract<ClientControlFrame, { kind: "get-scrollback-cells" }> {
  return { kind: "get-scrollback-cells", request_id: "req", session_id: SID, end_row: endRow, max_rows: maxRows };
}

describe("tail full frame + get-scrollback-cells backfill", () => {
  test("T1 — forced full frame carries only the scrollback tail", async () => {
    const frames: PbCellGridFrame[] = [];
    const mgr = freshMgr((f) => frames.push(f));
    const rec = await injectSession(mgr, SEED);
    const total = rec.wtermCore!.getScrollbackCount();
    expect(total).toBeGreaterThan(SB_SNAPSHOT_TAIL_ROWS);

    mgr.emitCellSnapshot(asChannelId(CID));
    expect(frames.length).toBe(1);
    const frame = protoToCellFrame(frames[0]!);
    expect(frame.full).toBe(true);
    expect(frame.scrollbackTotal).toBe(total);
    expect(frame.scrollbackRows.length).toBe(SB_SNAPSHOT_TAIL_ROWS);
    expect(frame.sbBase).toBe(total - SB_SNAPSHOT_TAIL_ROWS);
    expect(frame.scrollbackRows[0]!.index).toBe(frame.sbBase);
    expect(frame.scrollbackRows.at(-1)!.index).toBe(total - 1);
  });

  test("T2 — chunked backfill + tail reconstructs the untailed grid exactly", async () => {
    const mgr = freshMgr();
    const rec = await injectSession(mgr, SEED);
    const { coordLink, sent } = makeLinkCapture();
    const reference = gridToCellFrame(rec.wtermCore!, 1); // untailed: complete history
    const total = reference.scrollbackTotal;
    const sbBase = total - SB_SNAPSHOT_TAIL_ROWS;

    // Walk down from sbBase in 100-row chunks, newest→oldest (the SPA's order).
    const collected: CellRow[] = [];
    let end = sbBase;
    while (end > 0) {
      sent.length = 0;
      await handleGetScrollbackCells(cellsFrame(end, 100), "req", { coordLink, sessionMgr: mgr });
      expect(sent[0]!.kind).toBe("rpc-ok");
      const ok = sent[0] as RpcOk;
      expect(ok.data.end_row).toBe(end);
      expect(ok.data.total).toBe(total);
      expect(ok.data.cols).toBe(COLS);
      collected.unshift(...ok.data.rows);
      end = ok.data.start_row;
    }
    const reconstructed = collected.concat(reference.scrollbackRows.slice(sbBase));
    expect(reconstructed.length).toBe(total);
    // Row-for-row identity: indices contiguous, text identical at every seam.
    for (let i = 0; i < total; i++) {
      expect(reconstructed[i]!.index).toBe(i);
      expect(rowTextOf(reconstructed[i]!)).toBe(rowTextOf(reference.scrollbackRows[i]!));
    }
  });

  test("T3 — end_row clamps to the grid; unknown session errors", async () => {
    const mgr = freshMgr();
    const rec = await injectSession(mgr, SEED);
    const total = rec.wtermCore!.getScrollbackCount();
    const { coordLink, sent } = makeLinkCapture();

    await handleGetScrollbackCells(cellsFrame(999_999, 50), "req", { coordLink, sessionMgr: mgr });
    const ok = sent[0] as RpcOk;
    expect(ok.kind).toBe("rpc-ok");
    expect(ok.data.end_row).toBe(total);
    expect(ok.data.start_row).toBe(total - 50);
    expect(ok.data.rows.length).toBe(50);

    sent.length = 0;
    const ghost: Extract<ClientControlFrame, { kind: "get-scrollback-cells" }> = {
      kind: "get-scrollback-cells", request_id: "req",
      session_id: asSessionId("00000000-0000-0000-0000-00000000dead"),
      end_row: 10, max_rows: 10,
    };
    await handleGetScrollbackCells(ghost, "req", { coordLink, sessionMgr: mgr });
    expect(sent[0]!.kind).toBe("rpc-error");
  });

  test("T4 — a fetch racing a rebuild serves the post-rebuild grid", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, SEED);
    const { coordLink, sent } = makeLinkCapture();

    // Queue the deterministic rebuild at a NEW width, then fetch immediately —
    // the handler must await the chain and answer at the rebuilt width.
    const NEW_COLS = 40;
    mgr._scheduleWtermRebuild(CID, NEW_COLS, ROWS);
    await handleGetScrollbackCells(cellsFrame(50, 50), "req", { coordLink, sessionMgr: mgr });
    const ok = sent[0] as RpcOk;
    expect(ok.kind).toBe("rpc-ok");
    expect(ok.data.cols).toBe(NEW_COLS);
    // The rebuilt 40-col grid rewraps "line-N" rows; total grows vs the 80-col
    // grid only if lines exceed 40 cols (they don't) — but the swap must have
    // completed: the manager's core now reports the new width.
    expect(mgr.sessions.get(CID)!.wtermCore!.getCols()).toBe(NEW_COLS);
  });
});
