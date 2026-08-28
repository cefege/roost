// publishCellGrid maps a worker channel to its durable session binding, stamps
// that session_id on the proto frame, and delivers it directly to the installed
// TerminalScreenHub. Unmapped channels are dropped, just like PTY bytes.

import { describe, test, expect } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { PbCellGridFrameSchema } from "@roost/shared/proto/cell_pb";
import { asWorkerFp, asChannelId } from "@roost/shared/wire";
import { publishCellGrid, primeChannelMap } from "../src/byte-hub.ts";
import { installTerminalViewHub, TerminalViewHub } from "../src/connect/terminal-view-hub.ts";
import type { KyselyDB } from "../src/db/connection.ts";

const WF = asWorkerFp("ce".repeat(32));
const SID = "sess-cell-passthrough-1";

const STREAM_ID = "00000000-0000-4000-8000-000000000003";

function baseline(seq: bigint, gridEpoch: string) {
  return create(PbCellGridFrameSchema, {
    streamId: STREAM_ID,
    gridEpoch,
    cols: 10,
    rows: 2,
    full: true,
    seq,
    baseSeq: 0n,
    sbBase: 0n,
    scrollbackTotal: 0n,
    viewportRows: Array.from({ length: 2 }, (_, index) => ({ index, spans: [] })),
  });
}

describe("coord cell passthrough", () => {
  test("routes mapped cells through the installed screen hub and drops unmapped cells", () => {
    primeChannelMap([{ id: SID, worker_fp: String(WF), channel: 7 }]);

    const hub = new TerminalViewHub({
      db: null as unknown as KyselyDB,
      resolveRoute: async () => null,
      sendSnapshot: () => true,
    });
    const socketId = "cell-passthrough-socket";
    const got: Array<{
      sessionId: string;
      frameSessionId: string;
      streamId: string;
      gridEpoch: string;
    }> = [];
    hub.screen.registerSocket(socketId, {
      beginTerminalStream: () => true,
      enqueueTerminalState: () => {},
      replaceTerminalSnapshot: (sessionId, streamId, frames) => {
        const envelope = frames[0]?.frame;
        if (envelope?.case !== "cellGrid") return;
        got.push({
          sessionId,
          frameSessionId: envelope.value.sessionId,
          streamId,
          gridEpoch: envelope.value.gridEpoch,
        });
      },
      enqueueTerminalDelta: () => true,
      dropTerminalSession: () => {},
    });
    hub.screen.expectStream(SID, STREAM_ID, 10, 2);
    hub.screen.setWatching(socketId, SID, true);
    installTerminalViewHub(hub);

    try {
      const frame = baseline(1n, "worker-grid:3");
      publishCellGrid(WF, asChannelId(7), frame);
      expect(frame.sessionId).toBe(SID);
      expect(got).toEqual([{
        sessionId: SID,
        frameSessionId: SID,
        streamId: STREAM_ID,
        gridEpoch: "worker-grid:3",
      }]);

      const unmapped = baseline(2n, "worker-grid:4");
      publishCellGrid(WF, asChannelId(999), unmapped);
      expect(unmapped.sessionId).toBe("");
      expect(got).toHaveLength(1);
    } finally {
      hub.screen.unregisterSocket(socketId);
      installTerminalViewHub(null);
      hub.dispose();
    }
  });
});
