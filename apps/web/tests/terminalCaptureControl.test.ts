// Capture-control cases beyond the facade suite's acceptance set: arming writes no
// bundle, stop needs no consent, a partial capture stays downloadable locally, and a
// recorder- or worker-triggered incident is announced once with an authenticated
// download. Pure fixtures come from helpers/terminalCaptureSeam.ts; the mocks and
// awaited imports stay here, because `bun test --isolate` may start this body
// before a helper's own top-level awaits settle.

import { beforeEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import {
  captureResult,
  captureUiState,
  createCaptureSeamLog,
  createCaptureSeamState,
  downloadWorkerFileModule,
  drainSettle,
  emitLeaseState,
  resetCaptureSeam,
  SESSION_ID,
  terminalCaptureSeamModule,
} from "./helpers/terminalCaptureSeam.ts";
import type { TerminalCaptureMenuController } from "../src/components/terminal/terminalCaptureMenuController.ts";

const seam = createCaptureSeamState();
const seamLog = createCaptureSeamLog();
mock.module("../src/renderer/terminalIncidentCapture.ts", () => terminalCaptureSeamModule(seam, seamLog));
mock.module("../src/browser/downloadWorkerFile.ts", () => downloadWorkerFileModule(seamLog));
mock.module("../src/components/Settings/md/Button.tsx", () => ({
  Button: (props: Record<string, unknown>) => props.children,
}));
mock.module("../src/components/Settings/md/Dialog.tsx", () => ({
  Dialog: (props: Record<string, unknown>) => (props.open ? props.children : null),
}));
mock.module("../src/components/Settings/md/StatusDot.tsx", () => ({
  StatusDot: () => null,
}));

// Solid resolves to its SSR build under `bun test`; the client build owns the
// effect/cleanup semantics the controller's lease subscription relies on.
const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
mock.module("solid-js", () => Solid);
mock.module("react/jsx-dev-runtime", () => ({
  Fragment: Symbol("capture-control-fragment"),
  jsxDEV: (tag: unknown, props: Record<string, unknown> | null) => ({ tag, props }),
}));

const { createTerminalCaptureMenuController } = await import(
  "../src/components/terminal/terminalCaptureMenuController.ts"
);
const { toasts, clearToastsForAccountBoundary } = await import("../src/store/toastStore.ts");
const { _resetAnnouncedWorkerCaptures } = await import("../src/renderer/terminalCaptureDownload.ts");

/** The body runs after createRoot returns, so the controller's lease-subscription
 *  effect has already flushed and `emitLeaseState` is live. */
function withController(body: (controller: TerminalCaptureMenuController) => void): void {
  let dispose = () => {};
  let controller: TerminalCaptureMenuController | null = null;
  Solid.createRoot((disposeRoot) => {
    dispose = disposeRoot;
    controller = createTerminalCaptureMenuController(() => SESSION_ID);
  });
  if (!controller) throw new Error("controller was not created");
  try {
    body(controller);
  } finally {
    dispose();
  }
}

beforeEach(() => {
  resetCaptureSeam(seam, seamLog);
  clearToastsForAccountBoundary();
  _resetAnnouncedWorkerCaptures();
});

describe("terminal capture control", () => {
  test("confirmed consent arms the lease and writes no unrequested bundle", async () => {
    withController((controller) => {
      controller.requestStartDebugging();
      controller.confirmConsent();
    });
    await drainSettle();
    // The pre-arm freeze is the consent gate, not an incident: arming must not
    // spend the retention budget on evidence nobody asked for.
    expect(seamLog.order).toEqual(["freeze", `start:${SESSION_ID}`, "discard:frozen-1"]);
    expect(toasts().at(-1)?.msg).toBe("Terminal debugging recording");
  });

  test("a thrown transport fault reports the same shape as an RPC-level failure", async () => {
    seam.throwOnStart = true;
    withController((controller) => {
      controller.requestStartDebugging();
      controller.confirmConsent();
    });
    await drainSettle();
    const notice = toasts().at(-1);
    expect(notice?.kind).toBe("err");
    expect(notice?.msg).toBe("Terminal debugging could not start · internal");
    expect(notice?.details).toContain("error: internal");
    expect(notice?.details).toContain("browser evidence: retained");
    expect(seamLog.discarded).toEqual([]);
  });

  test("an unarmed manual capture sends only the consented frozen evidence", async () => {
    withController((controller) => {
      controller.requestCapture();
      expect(controller.consentKind()).toBe("capture");
      expect(seamLog.order).toEqual(["freeze"]);
      controller.confirmConsent();
    });
    await drainSettle();
    expect(seamLog.order).toEqual(["freeze", "capture:frozen-1"]);
    expect(seamLog.downloadedHrefs).toEqual([
      "/file/worker-fp/var/roost/terminal-incident-x.json.gz",
    ]);
    // The operator's own capture names what it is downloading, so it cannot be
    // confused with a worker-detected incident echoed on a lease response.
    expect(toasts().at(-1)?.msg).toContain("downloading terminal-incident-");
  });

  test("a partial capture keeps the frozen browser evidence downloadable locally", async () => {
    seam.uiState = captureUiState("recording");
    seam.frozenResult = captureResult("capture", "partial", { error: "worker_timeout" });
    withController((controller) => {
      controller.requestCapture();
    });
    await drainSettle();
    expect(seamLog.downloadedHrefs).toEqual([]);
    const notice = toasts().at(-1);
    expect(notice?.kind).toBe("warn");
    expect(notice?.msg).toBe("Terminal diagnostic captured (partial)");
    expect(notice?.details).toContain("worker bundle: unavailable");
    expect(notice?.details).toContain("error: worker_timeout");
    expect(notice?.details).toContain("browser evidence: held locally for retry");
    expect(notice?.action?.label).toBe("Download local evidence");
    notice?.action?.onClick();
    expect(seamLog.localDownloads).toEqual(["frozen-1"]);
  });

  test("stopping is independent of a saved bundle and needs no consent", async () => {
    seam.uiState = captureUiState("recording");
    withController((controller) => {
      controller.requestStopDebugging();
    });
    await drainSettle();
    expect(seamLog.order).toEqual([`stop:${SESSION_ID}`]);
    expect(toasts().at(-1)?.msg).toBe("Terminal debugging stopped · saved captures kept");
  });

  test("a worker-triggered capture is surfaced once with an authenticated download", async () => {
    seam.uiState = captureUiState("recording");
    seam.frozenResult = captureResult("capture", "captured", {
      path: null,
      recent_worker_capture: {
        capture_id: "33333333-3333-4333-8333-333333333333",
        path: "/var/roost/terminal-incident-worker.json.gz",
        byte_length: 44,
        status: "captured",
      },
    });
    withController((controller) => {
      controller.requestCapture();
    });
    await drainSettle();
    const notice = toasts().at(-1);
    // A worker-detected incident must not read like the operator's own capture.
    expect(notice?.msg).toBe("Worker detected a terminal incident");
    expect(notice?.details).toContain("saved on the worker");
    notice?.action?.onClick();
    await drainSettle();
    expect(seamLog.downloadedHrefs).toEqual([
      "/file/worker-fp/var/roost/terminal-incident-worker.json.gz",
    ]);
  });

  test("a recorder-triggered capture is announced once with a download action", () => {
    seam.uiState = captureUiState("recording");
    const automatic = captureResult("capture", "captured", {
      capture_id: "44444444-4444-4444-8444-444444444444",
      path: "/var/roost/terminal-incident-auto.json.gz",
      byte_length: 88,
    });
    withController(() => {
      emitLeaseState(seam, { ...captureUiState("recording"), lastResult: automatic });
      emitLeaseState(seam, { ...captureUiState("recording"), lastResult: automatic });

      const notices = toasts().filter((entry) => entry.msg === "Terminal diagnostic captured");
      expect(notices.length).toBe(1);
      notices[0]?.action?.onClick();
    });
    expect(seamLog.downloadedHrefs).toEqual([
      "/file/worker-fp/var/roost/terminal-incident-auto.json.gz",
    ]);
  });

  test("a lease-state echo of a manual capture is not announced twice", async () => {
    seam.uiState = captureUiState("recording");
    const manual = captureResult("capture", "captured", {
      path: "/var/roost/terminal-incident-x.json.gz",
      byte_length: 12,
    });
    seam.frozenResult = manual;
    withController((controller) => {
      controller.requestCapture();
      emitLeaseState(seam, { ...captureUiState("recording"), lastResult: manual });
    });
    await drainSettle();
    // Either spelling counts: the manual path names its download, the echoed
    // automatic path does not, and exactly one of them may be raised.
    const notices = toasts().filter((entry) => entry.msg.startsWith("Terminal diagnostic captured"));
    expect(notices.length).toBe(1);
  });
});
