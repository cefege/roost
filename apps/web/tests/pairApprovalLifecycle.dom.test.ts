// Approver lifecycle after PairApprove acknowledges: the code dialog stays up
// while the requester has not confirmed, retires itself on completion, and turns
// Cancel request into a server-side denial whose outcome the coordinator decides.
// Shares the provider fixture so RPCs, toasts, and the tab record are observed.

import { Code, ConnectError } from "@connectrpc/connect";
import { AUTH_LAYER_DEVICE, X_ROOST_AUTH_LAYER } from "@roost/shared/wire/headers";
import { afterAll, afterEach, beforeEach, describe, expect, test, vi } from "bun:test";
import {
  REQUEST_CODE,
  loadPairApprovalProviderFixture,
  pairApprovalStatus,
  pairApprove,
  pairDeny,
  resetPairApprovalFixture,
  restoreSessionStorage,
  rpc,
  sessionValues,
  settleApprovalWork,
  toasts,
} from "./helpers/pairApprovalProviderFixture.ts";
import type { MountedPairApprovalProvider } from "./helpers/pairApprovalProviderFixture.ts";

const { PAIR_APPROVAL_STORAGE_KEY, announcePairedBrowser, mount } =
  await loadPairApprovalProviderFixture();

const POLL_MS = 1_000;
const RETRY_MS = 10;
let nextRequestSerial = 1;
let requestId = "";
let mounted: MountedPairApprovalProvider | null = null;

/** Distinct per test: the paired-browser notice remembers announced ids. */
function freshRequestId(): string {
  return (nextRequestSerial++).toString(16).padStart(32, "0");
}

async function mountAwaitingConfirmation(): Promise<MountedPairApprovalProvider> {
  mounted = mount();
  await mounted.context.approve({
    ephemeralId: requestId,
    requesterLabel: "Kitchen tablet",
    expiresAtMs: Date.now() + 60_000,
  });
  expect(mounted.dialog()?.verificationCode).toBe(REQUEST_CODE);
  return mounted;
}

async function advance(ms: number): Promise<void> {
  vi.advanceTimersByTime(ms);
  await settleApprovalWork();
}

function toastMessages(): string[] {
  return toasts.map((toast) => toast.message);
}

function pairedNotices(): string[] {
  return toastMessages().filter((message) => message.startsWith("New browser paired"));
}

function expectRetired(provider: MountedPairApprovalProvider): void {
  expect(provider.dialog()).toBeNull();
  expect(provider.context.busyRequestId()).toBeNull();
  expect(sessionValues.has(PAIR_APPROVAL_STORAGE_KEY)).toBe(false);
}

beforeEach(() => {
  vi.useFakeTimers();
  resetPairApprovalFixture();
  requestId = freshRequestId();
});

afterEach(() => {
  mounted?.dispose();
  mounted = null;
  vi.clearAllTimers();
  vi.useRealTimers();
});

afterAll(restoreSessionStorage);

describe("PairApprovalProvider awaiting confirmation", () => {
  test("keeps the code while the requester has not confirmed", async () => {
    const provider = await mountAwaitingConfirmation();
    await advance(POLL_MS);
    await advance(POLL_MS);
    expect(pairApprovalStatus).toHaveBeenCalledTimes(2);
    expect(pairApprovalStatus).toHaveBeenLastCalledWith({ ceremonyVersion: 1, ephemeralId: requestId });
    expect(provider.dialog()).toMatchObject({ verificationCode: REQUEST_CODE, state: "awaiting" });
    expect(sessionValues.has(PAIR_APPROVAL_STORAGE_KEY)).toBe(true);
    expect(provider.context.busyRequestId()).toBe(requestId);
  });

  test("retires the dialog, record, and busy state once the requester confirms", async () => {
    rpc.status = async () => ({ status: "completed" });
    const provider = await mountAwaitingConfirmation();
    await advance(POLL_MS);
    expectRetired(provider);
    expect(pairedNotices()).toEqual(["New browser paired: Kitchen tablet"]);
    await advance(POLL_MS * 5);
    expect(pairApprovalStatus).toHaveBeenCalledTimes(1);
  });

  test("transient status failures retry with the code still shown", async () => {
    const failures: unknown[] = [
      new ConnectError("offline", Code.Unavailable),
      new TypeError("network failed"),
      new ConnectError("front door", Code.Unauthenticated),
    ];
    rpc.status = async () => {
      const failure = failures.shift();
      if (failure !== undefined) throw failure;
      return { status: "completed" };
    };
    const provider = await mountAwaitingConfirmation();
    await advance(POLL_MS);
    for (let retry = 0; retry < 3; retry++) {
      expect(provider.dialog()?.verificationCode).toBe(REQUEST_CODE);
      expect(sessionValues.has(PAIR_APPROVAL_STORAGE_KEY)).toBe(true);
      await advance(RETRY_MS);
    }
    expect(pairApprovalStatus).toHaveBeenCalledTimes(4);
    expectRetired(provider);
  });

  test("a classified device rejection ends approval authority", async () => {
    rpc.status = async () => {
      throw new ConnectError(
        "authentication required",
        Code.Unauthenticated,
        new Headers({ [X_ROOST_AUTH_LAYER]: AUTH_LAYER_DEVICE }),
      );
    };
    const provider = await mountAwaitingConfirmation();
    await advance(POLL_MS);
    expectRetired(provider);
    expect(toasts.at(-1)).toEqual({ message: "Pairing authority is no longer valid.", kind: "err" });
  });

  test("a stale client keeps the code, stops polling, and asks for a reload", async () => {
    rpc.status = async () => {
      throw new ConnectError("pairing client must reload", Code.FailedPrecondition);
    };
    const provider = await mountAwaitingConfirmation();
    const toastCount = toasts.length;
    await advance(POLL_MS);
    expect(provider.dialog()).toMatchObject({ verificationCode: REQUEST_CODE, state: "reload_required" });
    expect(sessionValues.has(PAIR_APPROVAL_STORAGE_KEY)).toBe(true);
    await advance(POLL_MS * 5);
    expect(pairApprovalStatus).toHaveBeenCalledTimes(1);
    expect(toasts.length).toBe(toastCount);

    provider.dialog()!.onCancel();
    await settleApprovalWork();
    expect(pairDeny).toHaveBeenCalledWith({ ephemeralId: requestId });
    expectRetired(provider);
    expect(toastMessages().at(-1)).toBe("Pairing request cancelled.");
  });

  test("shows one paired notice when Sync announces the completion first", async () => {
    rpc.status = async () => ({ status: "completed" });
    const provider = await mountAwaitingConfirmation();
    announcePairedBrowser({ ephemeralId: requestId, label: "Chrome on macOS · Berlin" });
    await advance(POLL_MS);
    expectRetired(provider);
    expect(pairedNotices()).toEqual(["New browser paired: Chrome on macOS · Berlin"]);
    announcePairedBrowser({ ephemeralId: requestId, label: "Chrome on macOS · Berlin" });
    expect(pairedNotices()).toHaveLength(1);
  });
});

describe("PairApprovalProvider cancel request", () => {
  test("every dismissal cancels once and discards a status read started before it", async () => {
    const staleStatus = Promise.withResolvers<{ status: string }>();
    const denial = Promise.withResolvers<{ ok: boolean }>();
    rpc.status = () => staleStatus.promise;
    rpc.deny = () => denial.promise;
    const provider = await mountAwaitingConfirmation();
    await advance(POLL_MS);
    expect(pairApprovalStatus).toHaveBeenCalledTimes(1);

    provider.dialog()!.onCancel();
    provider.dialog()!.onCancel();
    await settleApprovalWork();
    expect(pairDeny).toHaveBeenCalledTimes(1);
    expect(provider.dialog()).toMatchObject({ verificationCode: REQUEST_CODE, state: "cancelling" });

    staleStatus.resolve({ status: "denied" });
    await advance(POLL_MS * 3);
    expect(provider.dialog()?.state).toBe("cancelling");
    expect(pairApprovalStatus).toHaveBeenCalledTimes(1);
    expect(toastMessages()).not.toContain("Pairing request was denied.");

    denial.resolve({ ok: true });
    await settleApprovalWork();
    expectRetired(provider);
    expect(toastMessages().filter((message) => message === "Pairing request cancelled.")).toHaveLength(1);
    expect(pairApprove).toHaveBeenCalledTimes(1);
  });

  test("a reload mid-cancel never replays the approval", async () => {
    rpc.deny = () => Promise.withResolvers<{ ok: boolean }>().promise;
    const provider = await mountAwaitingConfirmation();
    provider.dialog()!.onCancel();
    await settleApprovalWork();
    provider.dispose();
    mounted = mount();
    await settleApprovalWork();
    expect(pairApprove).toHaveBeenCalledTimes(1);
    expect(mounted.dialog()).toBeNull();
  });

  test("a denial whose response was lost resolves through the status read", async () => {
    let denials = 0;
    rpc.deny = async () => {
      denials += 1;
      if (denials === 1) throw new TypeError("network failed");
      throw new ConnectError("not found", Code.NotFound);
    };
    rpc.status = async () => ({ status: "denied" });
    const provider = await mountAwaitingConfirmation();
    provider.dialog()!.onCancel();
    await settleApprovalWork();
    expect(provider.dialog()).toMatchObject({ verificationCode: REQUEST_CODE, state: "cancelling" });

    await advance(RETRY_MS);
    expect(pairDeny).toHaveBeenCalledTimes(2);
    expect(pairApprovalStatus).toHaveBeenCalledWith({ ceremonyVersion: 1, ephemeralId: requestId });
    expectRetired(provider);
    expect(toastMessages().at(-1)).toBe("Pairing request cancelled.");
    expect(pairApprove).toHaveBeenCalledTimes(1);
  });

  test("a completion that beat the cancel is reported as paired", async () => {
    rpc.deny = async () => {
      throw new ConnectError("not found", Code.NotFound);
    };
    rpc.status = async () => ({ status: "completed" });
    const provider = await mountAwaitingConfirmation();
    provider.dialog()!.onCancel();
    await settleApprovalWork();
    expectRetired(provider);
    expect(pairedNotices()).toEqual(["New browser paired: Kitchen tablet"]);
    expect(toastMessages()).not.toContain("Pairing request cancelled.");
  });
});
