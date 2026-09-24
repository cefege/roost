// Hover-target state for notification toasts: the ownership guard that keeps a
// superseded toast's late leave from clearing its successor's hold, the folder
// projection the sidebar rows read, and the toast-store dismissal freeze the
// same hover installs.

import { afterEach, beforeEach, describe, expect, test, vi } from "bun:test";
import { asChannelId, asSessionId, asWorkerFp } from "@roost/protocol/wire";
import type { Session } from "@roost/protocol/wire";
import { deleteStoreRecord, rootStore, setRootStore } from "../src/store/root.ts";
import { folderKeyOf } from "../src/lib/folderKey.ts";
import {
  holdNotifyTarget,
  notifyTargetFolderKey,
  notifyTargetSessionId,
  releaseNotifyTarget,
} from "../src/store/notifyTarget.ts";
import {
  addToast,
  clearToastsForAccountBoundary,
  holdToastDismiss,
  releaseToastDismiss,
  toasts,
} from "../src/store/toastStore.ts";

const SESSION_ID = "00000000-0000-4000-8000-0000000000a1";
const WORKER_FP = asWorkerFp("cc".repeat(32));

const targetSession: Session = {
  id: asSessionId(SESSION_ID),
  worker_fp: WORKER_FP,
  channel: asChannelId(1),
  kind: "shell",
  cwd: "/work/idea",
  spawn_cwd: "/work/idea",
  workspace_id: null,
  status: "open",
  created_at: 10,
  closed_at: null,
  custom_title: null,
};

beforeEach(() => {
  for (const sessionId of Object.keys(rootStore.sessions)) {
    deleteStoreRecord("sessions", sessionId);
  }
});

afterEach(() => {
  releaseNotifyTarget(1);
  releaseNotifyTarget(2);
  clearToastsForAccountBoundary();
});

describe("notify target hold", () => {
  test("only the owning toast can release the hold", () => {
    holdNotifyTarget(1, SESSION_ID);
    releaseNotifyTarget(2);
    expect(notifyTargetSessionId()).toBe(SESSION_ID);
    releaseNotifyTarget(1);
    expect(notifyTargetSessionId()).toBeNull();
  });

  test("a later hold survives the previous toast's late leave", () => {
    holdNotifyTarget(1, SESSION_ID);
    holdNotifyTarget(2, "other-session");
    releaseNotifyTarget(1);
    expect(notifyTargetSessionId()).toBe("other-session");
  });

  test("folder projection resolves through the session store", () => {
    holdNotifyTarget(1, SESSION_ID);
    expect(notifyTargetFolderKey()).toBeNull();
    setRootStore("sessions", SESSION_ID, targetSession);
    expect(notifyTargetFolderKey()).toBe(folderKeyOf(targetSession));
    releaseNotifyTarget(1);
    expect(notifyTargetFolderKey()).toBeNull();
  });
});

describe("toast dismissal freeze", () => {
  afterEach(() => { vi.useRealTimers(); });

  test("a hold banks the unelapsed ttl and release spends exactly that", () => {
    vi.useFakeTimers();
    addToast("hover me", "ok", { ttlMs: 50 });
    const id = toasts()[0]!.id;

    vi.advanceTimersByTime(30);
    holdToastDismiss(id);
    vi.advanceTimersByTime(500);
    expect(toasts().some((toast) => toast.id === id)).toBe(true);

    releaseToastDismiss(id);
    vi.advanceTimersByTime(19);
    expect(toasts().some((toast) => toast.id === id)).toBe(true);
    vi.advanceTimersByTime(2);
    expect(toasts().some((toast) => toast.id === id)).toBe(false);
  });

  test("hold and release are inert for a toast with no armed timer", () => {
    vi.useFakeTimers();
    addToast("sticky", "err", { ttlMs: null });
    const id = toasts()[0]!.id;
    holdToastDismiss(id);
    releaseToastDismiss(id);
    vi.advanceTimersByTime(60_000);
    expect(toasts().some((toast) => toast.id === id)).toBe(true);
  });
});
