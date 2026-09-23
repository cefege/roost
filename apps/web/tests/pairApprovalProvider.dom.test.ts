// Provider behavior coverage observes the tab record and RPC contract across
// discovery gating, ambiguous retry, and terminal cleanup. The shared fixture's
// JSX shim keeps the test independent of browser custom elements while
// preserving Solid state.

import { Code, ConnectError } from "@connectrpc/connect";
import { afterAll, afterEach, beforeEach, describe, expect, test, vi } from "bun:test";
import {
  REQUEST_CODE,
  REQUEST_ID,
  deletePairRequest,
  loadPairApprovalProviderFixture,
  pairApprove,
  resetPairApprovalFixture,
  restoreSessionStorage,
  rpc,
  sessionValues,
  settleApprovalWork,
} from "./helpers/pairApprovalProviderFixture.ts";

const { PAIR_APPROVAL_STORAGE_KEY, mount: mountPairApprovalProvider } =
  await loadPairApprovalProviderFixture();

beforeEach(() => {
  vi.useFakeTimers();
  resetPairApprovalFixture();
});

afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

afterAll(restoreSessionStorage);

describe("PairApprovalProvider", () => {
  test("persists a generated code before approval and only shows it after acknowledgement", async () => {
    const response = Promise.withResolvers<{ ok: boolean }>();
    rpc.approve = () => response.promise;
    const mounted = mountPairApprovalProvider();
    try {
      const approval = mounted.context.approve({
        ephemeralId: REQUEST_ID,
        requesterLabel: "Kitchen tablet",
        expiresAtMs: Date.now() + 60_000,
      });
      await settleApprovalWork();
      expect(pairApprove).toHaveBeenCalledWith({
        ceremonyVersion: 1,
        ephemeralId: REQUEST_ID,
        verificationCode: REQUEST_CODE,
      });
      expect(JSON.parse(sessionValues.get(PAIR_APPROVAL_STORAGE_KEY)!)).toMatchObject({
        ceremonyVersion: 1,
        ephemeralId: REQUEST_ID,
        verificationCode: REQUEST_CODE,
        requesterLabel: "Kitchen tablet",
      });
      expect(deletePairRequest).not.toHaveBeenCalled();
      expect(mounted.dialog()).toBeNull();
      expect(mounted.context.busyRequestId()).toBe(REQUEST_ID);

      response.resolve({ ok: true });
      await approval;
      expect(deletePairRequest).toHaveBeenCalledWith(REQUEST_ID);
      expect(mounted.dialog()).toMatchObject({
        verificationCode: REQUEST_CODE,
        requesterLabel: "Kitchen tablet",
        state: "awaiting",
      });
      expect(sessionValues.has(PAIR_APPROVAL_STORAGE_KEY)).toBe(true);
      expect(mounted.context.busyRequestId()).toBe(REQUEST_ID);
    } finally {
      mounted.dispose();
    }
  });

  test("defers persisted approval until discovery and retries an ambiguous result exactly", async () => {
    sessionValues.set(PAIR_APPROVAL_STORAGE_KEY, JSON.stringify({
      ceremonyVersion: 1,
      ephemeralId: REQUEST_ID,
      verificationCode: REQUEST_CODE,
      requesterLabel: "Kitchen tablet",
      expiresAtMs: Date.now() + 60_000,
    }));
    let attempts = 0;
    rpc.approve = async () => {
      attempts += 1;
      if (attempts === 1) throw new ConnectError("offline", Code.Unavailable);
      return { ok: true };
    };
    const mounted = mountPairApprovalProvider(false);
    try {
      await settleApprovalWork();
      expect(pairApprove).not.toHaveBeenCalled();
      mounted.setEnabled(true);
      await settleApprovalWork();
      expect(pairApprove).toHaveBeenCalledTimes(1);
      vi.advanceTimersByTime(10);
      await settleApprovalWork();
      expect(pairApprove).toHaveBeenCalledTimes(2);
      expect(pairApprove.mock.calls[0]?.[0]).toEqual(pairApprove.mock.calls[1]?.[0]);
      expect(pairApprove.mock.calls[1]?.[0]).toEqual({
        ceremonyVersion: 1,
        ephemeralId: REQUEST_ID,
        verificationCode: REQUEST_CODE,
      });
      expect(deletePairRequest).toHaveBeenCalledWith(REQUEST_ID);
      expect(mounted.dialog()?.verificationCode).toBe(REQUEST_CODE);
    } finally {
      mounted.dispose();
    }
  });

  test("clears expired and terminally rejected approval records", async () => {
    sessionValues.set(PAIR_APPROVAL_STORAGE_KEY, JSON.stringify({
      ceremonyVersion: 1,
      ephemeralId: REQUEST_ID,
      verificationCode: REQUEST_CODE,
      requesterLabel: "Kitchen tablet",
      expiresAtMs: Date.now() - 1,
    }));
    const expired = mountPairApprovalProvider();
    try {
      await settleApprovalWork();
      expect(pairApprove).not.toHaveBeenCalled();
      expect(sessionValues.has(PAIR_APPROVAL_STORAGE_KEY)).toBe(false);
    } finally {
      expired.dispose();
    }

    rpc.approve = async () => {
      throw new ConnectError("not found", Code.NotFound);
    };
    const rejected = mountPairApprovalProvider();
    try {
      await rejected.context.approve({
        ephemeralId: REQUEST_ID,
        requesterLabel: "Kitchen tablet",
        expiresAtMs: Date.now() + 60_000,
      });
      expect(sessionValues.has(PAIR_APPROVAL_STORAGE_KEY)).toBe(false);
      expect(rejected.context.busyRequestId()).toBeNull();
      expect(rejected.dialog()).toBeNull();
    } finally {
      rejected.dispose();
    }
  });
});
