// Direct view publication retries use the same document scheduler as Sync views.
// The fake registers through TerminalDirectRegistry and is promoted through a real staged baseline.
// These cases exercise refused active writes, not a retired local singleton or adapter aliases.
// The shared terminal fixture resets direct routes before and after every case.

import { create } from "@bufbuild/protobuf";
import { describe, expect, test, vi } from "bun:test";
import {
  TERMINAL_VIEW_HEARTBEAT_MS,
  TERMINAL_VIEW_LEASE_MS,
} from "@roost/shared/viewport";
import { LocalTerminalServerFrameSchema } from "@roost/shared/proto/local_terminal_pb";
import {
  TerminalViewStateFrameSchema,
  TerminalViewStatus,
} from "@roost/shared/proto/sync_pb";
import { terminalGenerationToken } from "../src/store/terminal-stream-liveness.ts";
import { _terminalViewRenewalSchedulerSnapshotForTest } from "../src/store/terminal-stream-renewal-scheduler.ts";
import { terminalDirectRegistry } from "../src/store/terminal-stream-transport.ts";
import { installRefusingDirectTransport } from "./helpers/refusingDirectTransport.ts";
import {
  CURRENT_SYNC_OWNER,
  SESSION_ID,
  STREAM_B,
  WORKER_FP,
  cellFrameToProto,
  full,
  latestViewCommand,
  row,
  setPageVisible,
  terminalStream,
} from "./helpers/terminalStreamFixture.ts";

function promoteRefusingDirect(cols: number, rows: number) {
  const direct = installRefusingDirectTransport(SESSION_ID, WORKER_FP);
  const candidate = terminalStream.createTerminalSessionPromotion({
    sessionId: SESSION_ID,
    attemptId: "refusing-direct-promotion",
    connection: direct.connection,
    token: direct.token,
  });
  if (!candidate) throw new Error("refusing direct candidate did not start");
  const command = direct.publishedViewCommands.at(-1);
  if (!command) throw new Error("refusing direct candidate did not publish a view");
  terminalStream.dispatchDirectTerminalFrame(direct.token, create(LocalTerminalServerFrameSchema, {
    frame: {
      case: "terminalViewState",
      value: create(TerminalViewStateFrameSchema, {
        viewId: command.viewId,
        sessionId: SESSION_ID,
        revision: command.revision,
        active: true,
        streamId: STREAM_B,
        status: TerminalViewStatus.ACCEPTED,
        effectiveCols: cols,
        effectiveRows: rows,
      }),
    },
  }));
  terminalStream.dispatchDirectTerminalFrame(direct.token, create(LocalTerminalServerFrameSchema, {
    frame: {
      case: "cellGrid",
      value: cellFrameToProto(full(
        STREAM_B,
        Array.from({ length: rows }, (_, index) => row(index, "x".repeat(cols))),
      ), SESSION_ID),
    },
  }));
  const prepared = candidate.prepare(
    "refusing-direct-route",
    terminalGenerationToken({ ...CURRENT_SYNC_OWNER, ready: true }),
  );
  if (!prepared || !terminalDirectRegistry.commitSessionPromotion(
    SESSION_ID,
    "refusing-direct-promotion",
    prepared,
  )) {
    direct.release();
    throw new Error("refusing direct candidate did not commit");
  }
  return direct;
}

describe("direct terminal view publication", () => {
  test("retries an active direct view whose socket write was refused", () => {
    const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    view.setViewport({ cols: 80, rows: 24 });
    const direct = promoteRefusingDirect(80, 24);
    try {
      direct.writeAccepted = false;
      const beforeRefusal = direct.publishedViewIds.length;
      view.setViewport({ cols: 81, rows: 24 });
      expect(direct.publishedViewIds).toHaveLength(beforeRefusal + 1);
      expect(_terminalViewRenewalSchedulerSnapshotForTest()).toMatchObject({
        armed: true,
        scheduledViewCount: 1,
      });

      vi.advanceTimersByTime(TERMINAL_VIEW_HEARTBEAT_MS);
      expect(direct.publishedViewIds).toHaveLength(beforeRefusal + 2);
      direct.writeAccepted = true;
      vi.advanceTimersByTime(TERMINAL_VIEW_HEARTBEAT_MS);
      expect(direct.publishedViewIds).toHaveLength(beforeRefusal + 3);
    } finally {
      direct.release();
    }
  });

  test("retries a refused direct view before its coordinator lease would expire", () => {
    const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    view.setViewport({ cols: 80, rows: 24 });
    const direct = promoteRefusingDirect(80, 24);
    try {
      direct.writeAccepted = false;
      view.setViewport({ cols: 81, rows: 24 });
      const refusedAttempts = direct.publishedViewIds.length;
      let elapsedMs = 0;
      while (
        direct.publishedViewIds.length === refusedAttempts
        && elapsedMs < TERMINAL_VIEW_LEASE_MS
      ) {
        vi.advanceTimersByTime(250);
        elapsedMs += 250;
      }
      expect(direct.publishedViewIds.length).toBeGreaterThan(refusedAttempts);
      expect(elapsedMs).toBeLessThan(TERMINAL_VIEW_LEASE_MS);
    } finally {
      direct.release();
    }
  });

  test("does not schedule hidden direct publication retries", () => {
    const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    view.setViewport({ cols: 80, rows: 24 });
    const direct = promoteRefusingDirect(80, 24);
    try {
      const beforeHiddenChange = direct.publishedViewIds.length;
      setPageVisible(false);
      view.setViewport({ cols: 81, rows: 24 });
      expect(direct.publishedViewIds).toHaveLength(beforeHiddenChange);
      expect(_terminalViewRenewalSchedulerSnapshotForTest()).toMatchObject({
        armed: false,
        scheduledViewCount: 0,
      });

      setPageVisible(true);
      const visiblePane = terminalStream.createTerminalView("session-second-pane", WORKER_FP);
      visiblePane.setViewport({ cols: 80, rows: 24 });
      vi.advanceTimersByTime(TERMINAL_VIEW_LEASE_MS);
      expect(direct.publishedViewIds).toHaveLength(beforeHiddenChange);
      expect(latestViewCommand().value.sessionId).toBe("session-second-pane");
    } finally {
      direct.release();
    }
  });
});
