// Pins global-search cursor binding, expiry, eviction, cancellation ordering,
// and the per-worker serialization lane independently of RPC transport.
// The production owners are instance-scoped; every test creates its own clock
// and state so no cursor or waiter can leak between cases.

import { describe, expect, test } from "bun:test";
import {
  GLOBAL_TERMINAL_SEARCH_CURSOR_TTL_MS,
  GLOBAL_TERMINAL_SEARCH_MAX_CURSORS_PER_DEVICE,
  GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
  GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
  GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
} from "@roost/shared/terminal-search";
import {
  _GLOBAL_SEARCH_MAX_ACTIVE_PER_DEVICE,
  _GLOBAL_SEARCH_MAX_CANCEL_TOMBSTONES_PER_DEVICE,
  GlobalSearchCursorOwner,
  type GlobalSearchCursorBinding,
  type GlobalSearchSessionPosition,
} from "../src/connect/global-search-cursors.ts";
import {
  _GLOBAL_SEARCH_MAX_WAITERS_PER_WORKER,
  GlobalSearchWorkerLaneOwner,
} from "../src/connect/global-search-worker-lanes.ts";
import { validateGlobalSearchGroupResult } from "../src/connect/global-search-fanout.ts";

const BINDING: GlobalSearchCursorBinding = {
  deviceFingerprint: "device-a",
  tabId: "tab-a",
  searchId: "search-a",
  query: "needle",
  caseSensitive: false,
  maxSessions: GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
  maxRowsPerSession: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
  maxMatches: GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
};

const POSITION: GlobalSearchSessionPosition = {
  sessionId: "00000000-0000-4000-8000-000000000001",
  workerFp: "a".repeat(64),
  gridEpoch: "epoch-a",
  beforeRow: 2_048,
};

const TEST_LIMITS = {
  maxSessions: GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
  maxRowsPerSession: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
  maxMatches: GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
};

describe("global search cursor owner", () => {
  test("binds opaque cursors to device, tab, search, and options", () => {
    const owner = new GlobalSearchCursorOwner();
    const cursor = owner.issueCursor({
      binding: BINDING,
      continuations: [{ position: POSITION, searched: false }],
      eligibleSessions: 1,
      searchedSessionIds: [POSITION.sessionId],
    });
    expect(cursor).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    const mismatches: GlobalSearchCursorBinding[] = [
      { ...BINDING, deviceFingerprint: "device-b" },
      { ...BINDING, tabId: "tab-b" },
      { ...BINDING, searchId: "search-b" },
      { ...BINDING, query: "other" },
      { ...BINDING, caseSensitive: true },
      { ...BINDING, maxSessions: 1 },
      { ...BINDING, maxRowsPerSession: 1 },
      { ...BINDING, maxMatches: 1 },
    ];
    for (const mismatch of mismatches) {
      expect(owner.claimCursor(cursor, mismatch)).toBeNull();
    }
    expect(owner.claimCursor(cursor, BINDING)).toEqual({
      sessions: [POSITION],
      eligibleSessions: 1,
      searchedSessionIds: [POSITION.sessionId],
    });
    expect(owner.claimCursor(cursor, BINDING)).toBeNull();
  });

  test("expires at sixty seconds and evicts the oldest fifth cursor per device", () => {
    let now = 10_000;
    let tokenIndex = 0;
    const tokens = Array.from(
      { length: GLOBAL_TERMINAL_SEARCH_MAX_CURSORS_PER_DEVICE + 2 },
      (_, index) => `00000000-0000-4000-8000-${String(index + 1).padStart(12, "0")}`,
    );
    const owner = new GlobalSearchCursorOwner({
      now: () => now,
      newToken: () => tokens[tokenIndex++]!,
    });
    for (
      let index = 0;
      index < GLOBAL_TERMINAL_SEARCH_MAX_CURSORS_PER_DEVICE + 1;
      index++
    ) {
      owner.issueCursor({
        binding: { ...BINDING, searchId: `search-${index}` },
        continuations: [{
          position: {
            ...POSITION,
            sessionId: `00000000-0000-4000-8000-${String(index + 20).padStart(12, "0")}`,
          },
          searched: false,
        }],
        eligibleSessions: 1,
        searchedSessionIds: [],
      });
    }
    expect(owner.claimCursor(tokens[0]!, { ...BINDING, searchId: "search-0" }))
      .toBeNull();
    expect(owner.claimCursor(tokens[1]!, { ...BINDING, searchId: "search-1" }))
      .not.toBeNull();

    const expiring = owner.issueCursor({
      binding: { ...BINDING, searchId: "expires" },
      continuations: [{ position: POSITION, searched: false }],
      eligibleSessions: 1,
      searchedSessionIds: [],
    });
    now += GLOBAL_TERMINAL_SEARCH_CURSOR_TTL_MS;
    expect(owner.claimCursor(expiring, { ...BINDING, searchId: "expires" }))
      .toBeNull();
  });

  test("retires before dispatch and releases in-flight cancellation only after send", () => {
    let now = 1;
    const owner = new GlobalSearchCursorOwner({ now: () => now });
    expect(owner.beginSearch(BINDING)).toBe("started");
    expect(owner.selectSessions(BINDING, [POSITION])).toBe(true);
    let cancellationReleased = false;
    owner.onCancel(BINDING, () => { cancellationReleased = true; });
    const cursor = owner.issueCursor({
      binding: BINDING,
      continuations: [{ position: POSITION, searched: false }],
      eligibleSessions: 1,
      searchedSessionIds: [],
    });

    const prepared = owner.prepareCancellation(BINDING);
    expect(prepared).toEqual({ shouldDispatch: true, selectedSessions: [POSITION] });
    expect(owner.beginSearch(BINDING)).toBe("cancelled");
    expect(owner.claimCursor(cursor, BINDING)).toBeNull();
    expect(cancellationReleased).toBe(false);
    owner.completeCancellation(BINDING);
    expect(cancellationReleased).toBe(true);
    expect(owner.prepareCancellation(BINDING).shouldDispatch).toBe(false);

    now += GLOBAL_TERMINAL_SEARCH_CURSOR_TTL_MS;
    expect(owner.beginSearch(BINDING)).toBe("started");
  });

  test("allows an unvisited cursor but rejects a row cursor without an epoch", () => {
    const owner = new GlobalSearchCursorOwner();
    expect(owner.issueCursor({
      binding: BINDING,
      continuations: [{
        position: { ...POSITION, gridEpoch: "", beforeRow: undefined },
        searched: false,
      }],
      eligibleSessions: 1,
      searchedSessionIds: [],
    })).toBeString();
    expect(() => owner.issueCursor({
      binding: BINDING,
      continuations: [{ position: { ...POSITION, gridEpoch: "" }, searched: false }],
      eligibleSessions: 1,
      searchedSessionIds: [],
    })).toThrow("row continuation requires a grid epoch");
    expect(() => owner.issueCursor({
      binding: BINDING,
      continuations: [
        { position: POSITION, searched: false },
        { position: POSITION, searched: false },
      ],
      eligibleSessions: 2,
      searchedSessionIds: [],
    })).toThrow("must be unique");
  });

  test("requires a searched session to resume strictly older than the row it was given", () => {
    const owner = new GlobalSearchCursorOwner();
    const requestedBeforeRow = POSITION.beforeRow!;
    const issue = (position: GlobalSearchSessionPosition, searched: boolean) =>
      owner.issueCursor({
        binding: BINDING,
        continuations: [{ position, searched, requestedBeforeRow }],
        eligibleSessions: 1,
        searchedSessionIds: [POSITION.sessionId],
      });
    expect(issue({ ...POSITION, beforeRow: requestedBeforeRow - 1 }, true)).toBeString();
    // An epoch reset restarts the session from its newest row: real progress
    // even though no row number survives.
    expect(issue({ ...POSITION, gridEpoch: "", beforeRow: undefined }, true)).toBeString();
    // A page that never reached the session may retry the same position.
    expect(issue(POSITION, false)).toBeString();
    for (const beforeRow of [requestedBeforeRow, requestedBeforeRow + 1]) {
      expect(() => issue({ ...POSITION, beforeRow }, true))
        .toThrow("must advance a searched session");
    }
  });

  test("keeps an eligible count larger than one page", () => {
    const owner = new GlobalSearchCursorOwner();
    const cursor = owner.issueCursor({
      binding: BINDING,
      continuations: [{ position: POSITION, searched: false }],
      eligibleSessions: BINDING.maxSessions * 4,
      searchedSessionIds: [POSITION.sessionId],
    });
    expect(owner.claimCursor(cursor, BINDING)?.eligibleSessions)
      .toBe(BINDING.maxSessions * 4);
    expect(() => owner.issueCursor({
      binding: BINDING,
      continuations: [{ position: POSITION, searched: false }],
      eligibleSessions: 0,
      searchedSessionIds: [],
    })).toThrow("requires bounded progress");
  });

  test("bounds active searches and cancellation tombstones per device", () => {
    const owner = new GlobalSearchCursorOwner();
    for (let index = 0; index < _GLOBAL_SEARCH_MAX_ACTIVE_PER_DEVICE; index++) {
      expect(owner.beginSearch({ ...BINDING, searchId: `active-${index}` }))
        .toBe("started");
    }
    expect(owner.beginSearch({ ...BINDING, searchId: "active-overflow" }))
      .toBe("capacity");

    const tombstoneOwner = new GlobalSearchCursorOwner();
    for (
      let index = 0;
      index <= _GLOBAL_SEARCH_MAX_CANCEL_TOMBSTONES_PER_DEVICE;
      index++
    ) {
      tombstoneOwner.prepareCancellation({
        ...BINDING,
        searchId: `cancel-${index}`,
      });
    }
    expect(tombstoneOwner.beginSearch({ ...BINDING, searchId: "cancel-0" }))
      .toBe("started");
    expect(tombstoneOwner.beginSearch({
      ...BINDING,
      searchId: `cancel-${_GLOBAL_SEARCH_MAX_CANCEL_TOMBSTONES_PER_DEVICE}`,
    })).toBe("cancelled");
  });
});

describe("global search worker lane", () => {
  test("serializes one worker while granting distinct workers in parallel", async () => {
    const lanes = new GlobalSearchWorkerLaneOwner();
    const signal = new AbortController().signal;
    const deadline = lanes.deadlineAfter(5_000);
    const first = await lanes.acquire("worker-a", deadline, signal);
    const otherWorker = await lanes.acquire("worker-b", deadline, signal);
    expect(first).not.toBeNull();
    expect(otherWorker).not.toBeNull();

    let secondGranted = false;
    const secondPromise = lanes.acquire("worker-a", deadline, signal).then((lease) => {
      secondGranted = true;
      return lease;
    });
    await Promise.resolve();
    expect(secondGranted).toBe(false);
    first!.release();
    const second = await secondPromise;
    expect(second).not.toBeNull();
    second!.release();
    otherWorker!.release();
  });

  test("removes an aborted queued waiter without disturbing the active lease", async () => {
    const lanes = new GlobalSearchWorkerLaneOwner();
    const firstSignal = new AbortController();
    const queuedSignal = new AbortController();
    const deadline = lanes.deadlineAfter(5_000);
    const first = await lanes.acquire("worker-a", deadline, firstSignal.signal);
    const queued = lanes.acquire("worker-a", deadline, queuedSignal.signal);
    queuedSignal.abort();
    expect(await queued).toBeNull();
    first!.release();
    expect(await lanes.acquire("worker-a", deadline, firstSignal.signal)).not.toBeNull();
  });
  test("rejects excess per-worker waiters before allocating timers", async () => {
    const lanes = new GlobalSearchWorkerLaneOwner();
    const signal = new AbortController();
    const deadline = lanes.deadlineAfter(5_000);
    const active = await lanes.acquire("worker-a", deadline, signal.signal);
    const queued = Array.from(
      { length: _GLOBAL_SEARCH_MAX_WAITERS_PER_WORKER },
      () => lanes.acquire("worker-a", deadline, signal.signal),
    );
    expect(await lanes.acquire("worker-a", deadline, signal.signal)).toBeNull();
    signal.abort();
    expect(await Promise.all(queued)).toEqual(
      Array(_GLOBAL_SEARCH_MAX_WAITERS_PER_WORKER).fill(null),
    );

    active!.release();
  });
});
test("rejects a same-epoch cursor result that skips the requested boundary", () => {
  const group = {
    workerFp: POSITION.workerFp,
    socket: {} as never,
    sessions: [POSITION],
    matchBudget: 8,
  };
  expect(validateGlobalSearchGroupResult({
    entries: [{
      status: "ok",
      session_id: POSITION.sessionId,
      result: {
        matches: [],
        truncated: false,
        scrollback_total: 3_000,
        cols: 80,
        grid_epoch: POSITION.gridEpoch,
        scanned_start_row: 0,
        scanned_end_row: 1_024,
        history_floor: "none",
        stop_reason: "complete",
      },
    }],
  }, group, TEST_LIMITS)).toBeNull();
});

test("accepts a same-epoch result stopped by a mid-scan epoch change", () => {
  const group = {
    workerFp: POSITION.workerFp,
    socket: {} as never,
    sessions: [POSITION],
    matchBudget: 8,
  };
  expect(validateGlobalSearchGroupResult({
    entries: [{
      status: "ok",
      session_id: POSITION.sessionId,
      result: {
        matches: [],
        truncated: false,
        scrollback_total: 3_000,
        cols: 80,
        grid_epoch: POSITION.gridEpoch,
        scanned_start_row: 1_548,
        scanned_end_row: POSITION.beforeRow,
        history_floor: "none",
        stop_reason: "epoch_changed",
      },
    }],
  }, group, TEST_LIMITS)).not.toBeNull();
});

test("rejects duplicate match identities from a worker batch", () => {
  const group = {
    workerFp: POSITION.workerFp,
    socket: {} as never,
    sessions: [POSITION],
    matchBudget: 8,
  };
  const match = { row: 2_000, col: 4, len: 6, preview: "needle" };
  expect(validateGlobalSearchGroupResult({
    entries: [{
      status: "ok",
      session_id: POSITION.sessionId,
      result: {
        matches: [match, match],
        truncated: false,
        scrollback_total: 3_000,
        cols: 80,
        grid_epoch: POSITION.gridEpoch,
        scanned_start_row: 0,
        scanned_end_row: POSITION.beforeRow,
        history_floor: "none",
        stop_reason: "complete",
      },
    }],
  }, group, TEST_LIMITS)).toBeNull();
});

