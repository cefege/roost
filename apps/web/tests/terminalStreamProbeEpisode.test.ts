// The idle probe reports an unpublishable-challenge EPISODE, never each retry:
// a Tier-1 line per retry would leave the always-on channel permanently red.
// These cases pin the edge, including the reset that liveness retirement owes
// it. terminalStream.test.ts keeps the repair and proof-deadline cases.

import { describe, expect, test, vi } from "bun:test";
import { setSignalSink } from "@roost/observability/diag";
import { TERMINAL_FOREGROUND_IDLE_PROBE_MS } from "@roost/protocol/viewport";
import {
  CURRENT_SYNC_OWNER,
  SESSION_ID,
  WORKER_FP,
  acceptView,
  cellFrameToProto,
  delta,
  full,
  latestViewCommand,
  setPageVisible,
  terminalStream,
  updateSyncState,
} from "./helpers/terminalStreamFixture.ts";

describe("terminal idle-probe rearm episodes", () => {
  test("reports a rearm episode again after liveness retirement", () => {
    const view = terminalStream.createTerminalView(SESSION_ID, WORKER_FP);
    view.setViewport({ cols: 1, rows: 1 });
    acceptView(view.viewId, latestViewCommand().value.revision as bigint);
    terminalStream.dispatchTerminalCellFrame(cellFrameToProto(full(), SESSION_ID));
    updateSyncState({ ...CURRENT_SYNC_OWNER, ready: false });
    let rearms = 0;
    setSignalSink((record) => {
      if (record.evt === "cell.foreground_stall" && record.action === "rearm") rearms += 1;
    });
    // diag coalesces a repeat of one kind|scope within 10s of Date.now(), so the
    // episodes straddle that window: a report suppressed by the cooldown would
    // green this case even with the retirement reset removed.
    let nowMs = Date.now() + 60_000;
    const nowSpy = vi.spyOn(Date, "now").mockImplementation(() => nowMs);
    try {
      vi.advanceTimersByTime(TERMINAL_FOREGROUND_IDLE_PROBE_MS * 2);
      expect(rearms).toBe(1); // the episode reports once, not once per retry

      // A hidden pane fails the probe's active-view guard, so the callback
      // retires liveness — the path that used to leave the edge latched.
      setPageVisible(false);
      vi.advanceTimersByTime(TERMINAL_FOREGROUND_IDLE_PROBE_MS);
      setPageVisible(true);
      nowMs += 60_000;
      terminalStream.dispatchTerminalCellFrame(cellFrameToProto(delta(2, "B"), SESSION_ID));
      vi.advanceTimersByTime(TERMINAL_FOREGROUND_IDLE_PROBE_MS);
      expect(rearms).toBe(2);
    } finally {
      setSignalSink(null);
      nowSpy.mockRestore();
      view.dispose();
    }
  });
});
