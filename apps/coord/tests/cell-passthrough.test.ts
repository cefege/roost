// cell-phase-2 — coord cell passthrough. publishCellGrid maps a worker channel to
// its session_id (byte-hub channel map), stamps it on the proto frame, and
// fans out via globalCellBus (which the Sync handler turns into
// FirehoseFrame.cell_grid). Unmapped channel = dropped, same as bytes.

import { describe, test, expect } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { PbCellGridFrameSchema } from "@roost/shared/proto/cell_pb";
import { asWorkerFp, asChannelId } from "@roost/shared/wire";
import { publishCellGrid, primeChannelMap } from "../src/byte-hub.ts";
import { globalCellBus } from "../src/buses.ts";

const WF = asWorkerFp("ce".repeat(32));
const SID = "sess-cell-passthrough-1";

describe("coord cell passthrough", () => {
  test("maps channel→session, stamps session_id, fans out; unmapped dropped", () => {
    primeChannelMap([{ id: SID, worker_fp: String(WF), channel: 7 }]);

    const got: Array<{ sessionId: string; gridEpoch: string }> = [];
    const unsub = globalCellBus.subscribe((f) => {
      if (f.sessionId === SID) got.push({ sessionId: f.sessionId, gridEpoch: f.gridEpoch });
    });
    got.length = 0;

    const frame = create(PbCellGridFrameSchema, {
      cols: 10, rows: 2, full: true, seq: 1n, gridEpoch: "worker-grid:3",
    });
    publishCellGrid(WF, asChannelId(7), frame);
    expect(frame.sessionId).toBe(SID);
    expect(got).toEqual([{ sessionId: SID, gridEpoch: "worker-grid:3" }]);

    // Unmapped channel → dropped (no fan-out).
    const before = got.length;
    publishCellGrid(WF, asChannelId(999), create(PbCellGridFrameSchema, { seq: 2n }));
    expect(got.length).toBe(before);

    unsub();
  });
});
