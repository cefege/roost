// Document-owned renewal serves active terminal views without per-view timers.
// These fake-clock cases fence disposal and generation replacement explicitly.
// Recovery must reassert scoped intent before it is allowed to redial Sync.

import { describe, expect, setSystemTime, test, vi } from "bun:test";
import {
  SESSION_ID,
  STREAM_A,
  acceptView,
  generationRecoveries,
  latestViewCommand,
  setPageFocused,
  terminalStream,
  updateSyncState,
  viewCommands,
} from "./helpers/terminalStreamFixture.ts";
import {
  _terminalViewRenewalSchedulerSnapshotForTest,
  scheduleTerminalViewRenewals,
} from "../src/store/terminal-stream-renewal-scheduler.ts";

describe("terminal document renewal scheduler", () => {
  test("serves many active views from one earliest deadline", () => {
    const first = terminalStream.createTerminalView(SESSION_ID);
    first.setViewport({ cols: 80, rows: 24 });
    acceptView(first.viewId, latestViewCommand().value.revision as bigint, STREAM_A, 80, 24);
    const second = terminalStream.createTerminalView(SESSION_ID);
    second.setViewport({ cols: 100, rows: 30 });
    acceptView(second.viewId, latestViewCommand().value.revision as bigint, STREAM_A, 100, 30);
    const initialCommandCount = viewCommands().length;

    expect(_terminalViewRenewalSchedulerSnapshotForTest()).toMatchObject({
      armed: true,
      scheduledViewCount: 2,
    });
    vi.advanceTimersByTime(5_000);

    const renewals = viewCommands().slice(initialCommandCount);
    expect(renewals.filter((command) => command.value.viewId === first.viewId)).toHaveLength(1);
    expect(renewals.filter((command) => command.value.viewId === second.viewId)).toHaveLength(1);
    expect(_terminalViewRenewalSchedulerSnapshotForTest()).toMatchObject({
      armed: true,
      scheduledViewCount: 2,
    });
    first.dispose();
    second.dispose();
  });

  test("cancels due callbacks on disposal and generation replacement", () => {
    const disposed = terminalStream.createTerminalView(SESSION_ID);
    disposed.setViewport({ cols: 80, rows: 24 });
    disposed.dispose();
    const afterDispose = viewCommands().length;
    expect(_terminalViewRenewalSchedulerSnapshotForTest()).toMatchObject({
      armed: false,
      scheduledViewCount: 0,
    });
    vi.advanceTimersByTime(5_000);
    expect(viewCommands()).toHaveLength(afterDispose);

    const replaced = terminalStream.createTerminalView(SESSION_ID);
    replaced.setViewport({ cols: 80, rows: 24 });
    updateSyncState({
      socketGeneration: 2,
      socketId: "socket-2",
      processEpoch: "process-2",
      domainGeneration: 12n,
      ready: false,
    });
    const afterReplacement = viewCommands().length;
    expect(_terminalViewRenewalSchedulerSnapshotForTest()).toMatchObject({
      armed: false,
      scheduledViewCount: 0,
    });
    vi.advanceTimersByTime(5_000);
    expect(viewCommands()).toHaveLength(afterReplacement);
    replaced.dispose();
  });

  test("renews after a wall-clock rollback", () => {
    const view = terminalStream.createTerminalView(SESSION_ID);
    view.setViewport({ cols: 80, rows: 24 });
    acceptView(view.viewId, latestViewCommand().value.revision as bigint, STREAM_A, 80, 24);
    const commandCount = viewCommands().length;
    try {
      setSystemTime(0);
      vi.advanceTimersByTime(5_000);
      expect(viewCommands()).toHaveLength(commandCount + 1);
    } finally {
      view.dispose();
      setSystemTime();
    }
  });

  test("renews a visible terminal even when its window is unfocused", () => {
    setPageFocused(false);
    const view = terminalStream.createTerminalView(SESSION_ID);
    view.setViewport({ cols: 80, rows: 24 });
    acceptView(view.viewId, latestViewCommand().value.revision as bigint, STREAM_A, 80, 24);
    const initialCommandCount = viewCommands().length;

    vi.advanceTimersByTime(5_000);

    expect(viewCommands()).toHaveLength(initialCommandCount + 1);
    expect(viewCommands().at(-1)!.value).toMatchObject({
      viewId: view.viewId,
      active: true,
      cols: 80,
      rows: 24,
    });
    view.dispose();
  });

  test("starts scoped repair before redialing an unanswered view", () => {
    const view = terminalStream.createTerminalView(SESSION_ID);
    view.setViewport({ cols: 80, rows: 24 });
    const initial = latestViewCommand().value;

    vi.advanceTimersByTime(15_000);
    expect(generationRecoveries).toHaveLength(0);
    expect(viewCommands().length).toBeGreaterThan(1);
    expect(viewCommands().at(-1)!.value).toMatchObject({
      viewId: view.viewId,
      revision: initial.revision,
      active: true,
      cols: 80,
      rows: 24,
    });

    vi.advanceTimersByTime(10_000);
    expect(generationRecoveries).toHaveLength(0);
    vi.advanceTimersByTime(34_999);
    expect(generationRecoveries).toHaveLength(0);
    vi.advanceTimersByTime(1);
    expect(generationRecoveries.at(-1)?.reason).toBe("terminal-proof-timeout");
    view.dispose();
  });
});
