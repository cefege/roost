// terminalSnapshotFacade — production-facing content-free terminal diagnostic reader
// plus the opt-in capture control the terminal menu drives through it.
// Mocks keep UUID admission, field filtering, RPC propagation, and JSON rejection isolated;
// the shared seam fixture makes consent ordering (freeze → confirm → send) observable.
// The window installation is explicit because main.tsx owns production bootstrap order.

import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import {
  captureResult,
  captureUiState,
  createCaptureSeamLog,
  createCaptureSeamState,
  downloadWorkerFileModule,
  drainSettle,
  resetCaptureSeam,
  SESSION_ID,
  terminalCaptureSeamModule,
} from "../helpers/terminalCaptureSeam.ts";
import type { TerminalCaptureMenuController } from "../../src/components/terminal/terminalCaptureMenuController.ts";

// Mock registration and the awaited imports stay in the suite: under
// `bun test --isolate` a helper's own top-level awaits can settle after this
// body starts running, which would hand the modules under test a real seam.
const seam = createCaptureSeamState();
const seamLog = createCaptureSeamLog();
mock.module("../../src/renderer/terminalIncidentCapture.ts", () => terminalCaptureSeamModule(seam, seamLog));
mock.module("../../src/browser/downloadWorkerFile.ts", () => downloadWorkerFileModule(seamLog));
mock.module("../../src/components/Settings/md/Button.tsx", () => ({
  Button: (props: Record<string, unknown>) => props.children,
}));
mock.module("../../src/components/Settings/md/Dialog.tsx", () => ({
  Dialog: (props: Record<string, unknown>) => (props.open ? props.children : null),
}));
mock.module("../../src/components/Settings/md/StatusDot.tsx", () => ({
  StatusDot: () => null,
}));

// Solid resolves to its SSR build under `bun test`; the client build owns the
// effect/cleanup semantics the controller's lease subscription relies on.
const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
mock.module("solid-js", () => Solid);
mock.module("react/jsx-dev-runtime", () => ({
  Fragment: Symbol("facade-capture-fragment"),
  jsxDEV: (tag: unknown, props: Record<string, unknown> | null) => ({ tag, props }),
}));

type DiagSnapshotRequest = {
  sessionFilterIds: string[];
  spaStateJson: string;
};

type TestWindow = {
  __roostTerminalSnapshot?: (sessionId: string) => Promise<{
    browser: Record<string, unknown>;
    diagnostic: unknown;
  }>;
};

let testWindow: TestWindow;

const diagRequests: DiagSnapshotRequest[] = [];
let diagnosticResponse: { snapshotJson: unknown } | Error = { snapshotJson: "{}" };
let browserSnapshot: Record<string, unknown> = {};

// The whole module is replaced, so every export the store graph reaches for
// must exist; a partial mock leaks a broken connect module into sibling suites
// sharing this process.
mock.module("../../src/client/rpc/connect.ts", () => ({
  classifyAuthFailure: () => "retryable",
  coordBase: () => "http://coord.test",
  coordinatorBaseUrl: () => "http://coord.test",
  coordinatorRpcUrl: (path: string) => `http://coord.test${path}`,
  reconcileCoordinatorOverrideAfterDiscovery: () => false,
  makeCoordinatorClientForSigner: () => ({}),
  publicCoordClient: {},
  coordClient: {
    async diagSnapshot(request: DiagSnapshotRequest) {
      diagRequests.push(request);
      if (diagnosticResponse instanceof Error) throw diagnosticResponse;
      return diagnosticResponse;
    },
  },
}));

mock.module("../../src/renderer/terminalDiagSnapshot.ts", () => ({
  terminalBrowserStreamSnapshot() {
    return browserSnapshot;
  },
}));

// Mock registration must precede this facade import because it binds every
// dependency at evaluation.
const {
  installTerminalSnapshotFacade,
  startTerminalCapture,
  captureTerminalIncident,
  stopTerminalCapture,
} = await import("../../src/renderer/terminalSnapshotFacade.ts");
const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
const { createTerminalCaptureMenuController } = await import(
  "../../src/components/terminal/terminalCaptureMenuController.ts"
);
const { toasts, clearToastsForAccountBoundary } = await import("../../src/store/toastStore.ts");
const { _resetAnnouncedWorkerCaptures } = await import("../../src/renderer/terminalCaptureDownload.ts");

/** The body runs after createRoot returns, so the controller's lease-subscription
 *  effect has already flushed. */
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

function installedSnapshotReader(): NonNullable<TestWindow["__roostTerminalSnapshot"]> {
  const reader = testWindow.__roostTerminalSnapshot;
  if (!reader) throw new Error("terminal snapshot facade was not installed");
  return reader;
}

function snapshotFixture(): Record<string, unknown> {
  return {
    session_id: SESSION_ID,
    captured_at_ms: 123,
    build: { git_sha: "build-sha" },
    wire_received: { stream_id: "stream", grid_epoch: "epoch", seq: 9 },
    replica: { expected_stream_id: "stream", grid_epoch: "epoch", seq: 9 },
    view: { view_id: "view", stream_id: "stream" },
    sync: { socket_generation: 2, socket_id: "socket", process_epoch: "process", ready: true },
    route: {
      active: {
        kind: "loopback",
        worker_epoch: "worker-epoch",
        peer_id: null,
        phase: "active",
        candidate_type: "none",
        probe_age_ms: 12,
        rtt_ms: null,
        worker_control_rtt_ms: 1,
        buffered_bytes: 0,
        answer_sdp: "ACTIVE-SDP-MUST-NOT-LEAK",
      },
      candidate: {
        kind: "webrtc",
        worker_epoch: "worker-epoch",
        peer_id: "opaque-peer-id",
        phase: "candidate",
        candidate_type: "host",
        probe_age_ms: null,
        rtt_ms: null,
        worker_control_rtt_ms: null,
        buffered_bytes: 0,
        address: "CANDIDATE-ADDRESS-MUST-NOT-LEAK",
      },
      peer_phase: "candidate",
      fallback_reason: null,
      pending_input_count: 0,
      offer_sdp: "DIRECT-SDP-MUST-NOT-LEAK",
      address: "DIRECT-ADDRESS-MUST-NOT-LEAK",
      credential: "DIRECT-CREDENTIAL-MUST-NOT-LEAK",
      terminal_content: "DIRECT-TERMINAL-CONTENT-MUST-NOT-LEAK",
    },
    handler_canonical: { grid_epoch: "epoch", seq: 9 },
    dom_reconciled: { grid_epoch: "epoch", seq: 9 },
    reconcile_block_reason: null,
    presentation: { canonical: { grid_epoch: "epoch", seq: 9 } },
    slot: { registered: true, connected: true },
    visibility: { document_visible: true, page_visible: true },
    faults: { private_fault: "excluded" },
    history: { private_history: "excluded" },
    last_geometry_proof: { marker: "TERMINAL-CONTENT-MUST-NOT-LEAK" },
  };
}

beforeEach(() => {
  diagRequests.length = 0;
  diagnosticResponse = { snapshotJson: JSON.stringify({ coord: { ok: true } }) };
  browserSnapshot = snapshotFixture();
  testWindow = {};
  Object.defineProperty(globalThis, "window", { configurable: true, value: testWindow });
  installTerminalSnapshotFacade();
  resetCaptureSeam(seam, seamLog);
  clearToastsForAccountBoundary();
  _resetAnnouncedWorkerCaptures();
});

afterEach(() => {
  diagRequests.length = 0;
});

afterAll(() => {
  if (originalWindow) Object.defineProperty(globalThis, "window", originalWindow);
  else Reflect.deleteProperty(globalThis, "window");
});

describe("terminal production snapshot facade", () => {
  test("rejects an invalid UUID before coordinator RPC", async () => {
    await expect(installedSnapshotReader()("not-a-terminal-uuid"))
      .rejects.toThrow("invalid terminal snapshot session id");
    expect(diagRequests).toEqual([]);
  });

  test("sends and returns only the production content-free field pick", async () => {
    const result = await installedSnapshotReader()(SESSION_ID);
    expect(Object.keys(result.browser).sort()).toEqual([
      "build",
      "captured_at_ms",
      "dom_reconciled",
      "handler_canonical",
      "presentation",
      "reconcile_block_reason",
      "replica",
      "route",
      "session_id",
      "slot",
      "sync",
      "view",
      "visibility",
      "wire_received",
    ]);
    expect(result.browser.route).toEqual({
      active: {
        kind: "loopback",
        worker_epoch: "worker-epoch",
        peer_id: null,
        phase: "active",
        candidate_type: "none",
        probe_age_ms: 12,
        rtt_ms: null,
        worker_control_rtt_ms: 1,
        buffered_bytes: 0,
      },
      candidate: {
        kind: "webrtc",
        worker_epoch: "worker-epoch",
        peer_id: "opaque-peer-id",
        phase: "candidate",
        candidate_type: "host",
        probe_age_ms: null,
        rtt_ms: null,
        worker_control_rtt_ms: null,
        buffered_bytes: 0,
      },
      peer_phase: "candidate",
      fallback_reason: null,
      pending_input_count: 0,
    });
    expect(result.browser).not.toHaveProperty("faults");
    expect(result.browser).not.toHaveProperty("history");
    expect(result.browser).not.toHaveProperty("last_geometry_proof");
    expect(JSON.stringify(result.browser)).not.toContain("DIRECT-SDP-MUST-NOT-LEAK");
    expect(JSON.stringify(result.browser)).not.toContain("DIRECT-ADDRESS-MUST-NOT-LEAK");
    expect(JSON.stringify(result.browser)).not.toContain("DIRECT-CREDENTIAL-MUST-NOT-LEAK");
    expect(JSON.stringify(result.browser)).not.toContain("DIRECT-TERMINAL-CONTENT-MUST-NOT-LEAK");
    expect(JSON.stringify(result.browser)).not.toContain("ACTIVE-SDP-MUST-NOT-LEAK");
    expect(JSON.stringify(result.browser)).not.toContain("CANDIDATE-ADDRESS-MUST-NOT-LEAK");
    expect(result.diagnostic).toEqual({ coord: { ok: true } });
    expect(diagRequests).toEqual([{
      sessionFilterIds: [SESSION_ID],
      spaStateJson: JSON.stringify(result.browser),
    }]);
  });

  test("preserves RPC errors and rejects empty or malformed diagnostic JSON", async () => {
    const rpcError = new Error("coordinator unavailable");
    diagnosticResponse = rpcError;
    await expect(installedSnapshotReader()(SESSION_ID)).rejects.toBe(rpcError);

    diagnosticResponse = { snapshotJson: "" };
    await expect(installedSnapshotReader()(SESSION_ID))
      .rejects.toThrow("terminal diagnostic RPC returned empty snapshotJson");

    diagnosticResponse = { snapshotJson: "not-json" };
    await expect(installedSnapshotReader()(SESSION_ID))
      .rejects.toThrow("terminal diagnostic RPC returned invalid snapshotJson");
  });
});

describe("terminal capture control through the facade", () => {
  test("installs exactly one content-free window reader and no command evaluator", () => {
    expect(Object.keys(testWindow)).toEqual(["__roostTerminalSnapshot"]);
  });

  test("capture APIs reach the recorder seam without a coordinator snapshot RPC", async () => {
    await startTerminalCapture(SESSION_ID);
    await captureTerminalIncident(SESSION_ID, "history_identity");
    await stopTerminalCapture(SESSION_ID);
    expect(seamLog.order).toEqual([
      `start:${SESSION_ID}`,
      `capture-live:${SESSION_ID}:history_identity`,
      `stop:${SESSION_ID}`,
    ]);
    expect(diagRequests).toEqual([]);
  });

  test("an armed capture freezes evidence before the capture call", async () => {
    seam.uiState = captureUiState("recording");
    withController((controller) => {
      controller.requestCapture();
      // settle() runs synchronously up to its await, so both calls land in this
      // tick; what matters is that the freeze strictly precedes the send and no
      // result has been reported yet.
      expect(seamLog.freezes).toEqual([{ sessionId: SESSION_ID, reason: "manual" }]);
      expect(seamLog.order.indexOf("freeze"))
        .toBeLessThan(seamLog.order.indexOf("capture:frozen-1"));
      expect(toasts()).toEqual([]);
    });
    await drainSettle();
  });

  test("cancelled consent sends nothing and discards the frozen token", async () => {
    withController((controller) => {
      controller.requestStartDebugging();
      expect(controller.consentKind()).toBe("start");
      expect(seamLog.order).toEqual(["freeze"]);

      controller.cancelConsent();
      expect(controller.consentKind()).toBe(null);
    });
    await drainSettle();
    expect(seamLog.discarded).toEqual(["frozen-1"]);
    expect(seamLog.order).toEqual(["freeze", "discard:frozen-1"]);
  });

  test("a partial failure keeps the frozen evidence and surfaces the fixed error code", async () => {
    seam.startResult = captureResult("start", "partial", { error: "worker_offline" });
    withController((controller) => {
      controller.requestStartDebugging();
      controller.confirmConsent();
    });
    await drainSettle();
    expect(seamLog.order).toEqual(["freeze", `start:${SESSION_ID}`]);
    expect(seamLog.discarded).toEqual([]);
    const notice = toasts().at(-1);
    expect(notice?.kind).toBe("err");
    expect(notice?.msg).toBe("Terminal debugging could not start · worker_offline");
    expect(notice?.details).toContain("error: worker_offline");
  });

  test("an expired lease is never auto-renewed", () => {
    seam.uiState = captureUiState("expired");
    withController((controller) => {
      expect(controller.captureState().phase).toBe("expired");
      expect(controller.startDisabled()).toBe(false);
      expect(seamLog.order).toEqual([]);

      // Renewal requires the explicit consent-bearing START again.
      controller.requestStartDebugging();
      expect(controller.consentKind()).toBe("start");
      expect(seamLog.order).toEqual(["freeze"]);
    });
  });
});
