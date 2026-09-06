// Focused contract tests for acknowledged browser layout application.
// They pin exact tab/socket targeting, pre-mutation validation, one commit,
// bounded settlement diagnostics, result ordering, and the no-bridge response.
// The eight legacy UI commands remain outside this execution-ACK path.

import { describe, expect, test } from "bun:test";
import {
  UiApplyLayoutOutcome,
  type UiCommandFrame,
} from "@roost/shared/proto/sync_pb";
import {
  UI_LAYOUT_APPLY_REJECTION,
  executeTargetedUiLayoutApply,
  rejectTargetedUiLayoutApplyWithoutBridge,
  type UiLayoutApplyDependencies,
  type UiLayoutApplyFolder,
  type UiLayoutApplyResult,
  type UiLayoutApplySettlementDiagnostic,
} from "../src/lib/uiLayoutApplyCore.ts";
import { applyLayoutDocument } from "../src/store/paneLayoutDocument.ts";
import { _paneLayoutStoreDebugSnapshot } from "../src/store/paneLayoutStore.ts";

const SESSION_ID = "00000000-0000-4000-8000-000000000001";
const PENDING_SESSION_ID = "00000000-0000-4000-8000-000000000002";

function applyFrame(overrides: Partial<UiCommandFrame> = {}): UiCommandFrame {
  return {
    targetTabId: "tab-current",
    targetSocketId: "socket-current",
    correlationId: "correlation-1",
    command: {
      command: {
        case: "applyLayout",
        value: { document: {} },
      },
    },
    ...overrides,
  } as unknown as UiCommandFrame;
}

interface CapturedSettlementDiagnostic {
  readonly event: string;
  readonly payload: UiLayoutApplySettlementDiagnostic;
}

interface ApplyHarness {
  readonly dependencies: UiLayoutApplyDependencies;
  readonly events: string[];
  readonly results: UiLayoutApplyResult[];
  readonly settlements: CapturedSettlementDiagnostic[];
  readonly counts: { decode: number; apply: number; mutations: number };
  setActiveFolder(folder: UiLayoutApplyFolder | null): void;
  setDecodeFailure(fails: boolean): void;
  setApplyFailure(fails: boolean): void;
  setNavigationFailure(fails: boolean): void;
  setResultAccepted(accepted: boolean): void;
  setIdentityReplacement(identity: "socket" | "tab" | null): void;
}

function createHarness(): ApplyHarness {
  let socketId: string | null = "socket-current";
  let tabId = "tab-current";
  let activeFolder: UiLayoutApplyFolder | null = {
    folderKey: "worker::/work",
    activeSessionId: SESSION_ID,
    liveSessionIds: [SESSION_ID],
    hasClientOnlySession: false,
  };
  let decodeFailure = false;
  let applyFailure = false;
  let navigationFailure = false;
  let identityReplacement: "socket" | "tab" | null = null;
  let resultAccepted = true;
  const events: string[] = [];
  const results: UiLayoutApplyResult[] = [];
  const settlements: CapturedSettlementDiagnostic[] = [];
  const counts = { decode: 0, apply: 0, mutations: 0 };
  const dependencies: UiLayoutApplyDependencies = {
    currentTabId: () => tabId,
    currentSocketId: () => socketId,
    activeFolder: () => activeFolder,
    decodeDocument: () => {
      counts.decode += 1;
      events.push("decode");
      if (decodeFailure) throw new Error(`private parser detail ${SESSION_ID}`);
      return { checked: true };
    },
    applyDocument: (folderKey, document, liveSessionIds) => {
      counts.apply += 1;
      events.push("apply");
      expect(document).toEqual({ checked: true });
      expect(liveSessionIds).toEqual([SESSION_ID]);
      if (applyFailure) throw new Error(`private membership detail ${SESSION_ID}`);
      counts.mutations += 1;
      events.push(`commit:${folderKey}`);
      if (identityReplacement === "socket") socketId = "socket-replacement";
      if (identityReplacement === "tab") tabId = "tab-replacement";
      return { selectedSessionId: SESSION_ID };
    },
    clearSpotlight: () => { events.push("spotlight"); },
    navigateToSession: (sessionId) => {
      events.push(`navigate:${sessionId}`);
      if (navigationFailure) throw new Error("router detail");
    },
    sendResult: (result) => {
      events.push("ack");
      results.push(result);
      return resultAccepted;
    },
    recordDiagnostic: (event, payload) => {
      settlements.push({ event, payload });
    },
  };
  return {
    dependencies,
    events,
    results,
    settlements,
    counts,
    setActiveFolder: (next) => { activeFolder = next; },
    setDecodeFailure: (fails) => { decodeFailure = fails; },
    setApplyFailure: (fails) => { applyFailure = fails; },
    setNavigationFailure: (fails) => { navigationFailure = fails; },
    setIdentityReplacement: (identity) => { identityReplacement = identity; },
    setResultAccepted: (accepted) => { resultAccepted = accepted; },
  };
}

describe("acknowledged UI layout apply", () => {
  test("commits exactly once, then clears spotlight, navigates, and ACKs applied", () => {
    const harness = createHarness();
    expect(executeTargetedUiLayoutApply(applyFrame(), harness.dependencies)).toBe(true);
    expect(harness.counts).toEqual({ decode: 1, apply: 1, mutations: 1 });
    expect(harness.events).toEqual([
      "decode",
      "apply",
      "commit:worker::/work",
      "spotlight",
      `navigate:${SESSION_ID}`,
      "ack",
    ]);
    expect(harness.results).toEqual([{
      correlationId: "correlation-1",
      outcome: UiApplyLayoutOutcome.APPLIED,
      reason: undefined,
    }]);
  });

  test("records one bounded correlation-only diagnostic for each outcome", () => {
    const applied = createHarness();
    executeTargetedUiLayoutApply(applyFrame(), applied.dependencies);
    const rejected = createHarness();
    rejected.setDecodeFailure(true);
    executeTargetedUiLayoutApply(applyFrame(), rejected.dependencies);
    const longCorrelation = "🦆".repeat(129);
    const bounded = createHarness();
    bounded.setResultAccepted(false);
    executeTargetedUiLayoutApply(
      applyFrame({ correlationId: longCorrelation }),
      bounded.dependencies,
    );

    expect(applied.settlements).toEqual([{
      event: "ui_cc.layout_apply_settled",
      payload: { correlation_id: "correlation-1", outcome: "applied" },
    }]);
    expect(rejected.settlements).toEqual([{
      event: "ui_cc.layout_apply_settled",
      payload: { correlation_id: "correlation-1", outcome: "rejected" },
    }]);
    expect(bounded.results[0]?.correlationId).toBe(longCorrelation);
    expect(bounded.settlements).toEqual([{
      event: "ui_cc.layout_apply_settled",
      payload: { correlation_id: "🦆".repeat(128), outcome: "applied" },
    }]);
    for (const settlement of [
      ...applied.settlements,
      ...rejected.settlements,
      ...bounded.settlements,
    ]) {
      expect(Object.keys(settlement.payload).sort()).toEqual([
        "correlation_id",
        "outcome",
      ]);
    }
  });

  test("rejects absent or non-live active folders before decoding or mutation", () => {
    for (const folder of [
      null,
      {
        folderKey: "worker::/work",
        activeSessionId: SESSION_ID,
        liveSessionIds: [],
        hasClientOnlySession: false,
      },
    ]) {
      const harness = createHarness();
      harness.setActiveFolder(folder);
      expect(executeTargetedUiLayoutApply(applyFrame(), harness.dependencies)).toBe(true);
      expect(harness.counts).toEqual({ decode: 0, apply: 0, mutations: 0 });
      expect(harness.events).toEqual(["ack"]);
      expect(harness.results[0]).toEqual({
        correlationId: "correlation-1",
        outcome: UiApplyLayoutOutcome.REJECTED,
        reason: UI_LAYOUT_APPLY_REJECTION.noActiveFolder,
      });
    }
  });

  test("rejects active or sibling optimistic membership before mutation", () => {
    const folders: UiLayoutApplyFolder[] = [
      {
        folderKey: "worker::/work",
        activeSessionId: PENDING_SESSION_ID,
        liveSessionIds: [PENDING_SESSION_ID],
        hasClientOnlySession: true,
      },
      {
        folderKey: "worker::/work",
        activeSessionId: SESSION_ID,
        liveSessionIds: [SESSION_ID, PENDING_SESSION_ID],
        hasClientOnlySession: true,
      },
    ];
    for (const folder of folders) {
      const harness = createHarness();
      harness.setActiveFolder(folder);
      const storeBefore = _paneLayoutStoreDebugSnapshot();
      executeTargetedUiLayoutApply(applyFrame(), {
        ...harness.dependencies,
        applyDocument: applyLayoutDocument,
      });
      expect(harness.counts).toEqual({ decode: 0, apply: 0, mutations: 0 });
      expect(_paneLayoutStoreDebugSnapshot()).toEqual(storeBefore);
      expect(harness.results[0]).toEqual({
        correlationId: "correlation-1",
        outcome: UiApplyLayoutOutcome.REJECTED,
        reason: UI_LAYOUT_APPLY_REJECTION.noActiveFolder,
      });
      expect(harness.settlements[0]).toEqual({
        event: "ui_cc.layout_apply_settled",
        payload: { correlation_id: "correlation-1", outcome: "rejected" },
      });
    }
  });

  test("sanitizes document and live-membership validation failures without mutation", () => {
    const parserFailure = createHarness();
    parserFailure.setDecodeFailure(true);
    executeTargetedUiLayoutApply(applyFrame(), parserFailure.dependencies);
    expect(parserFailure.counts).toEqual({ decode: 1, apply: 0, mutations: 0 });
    expect(parserFailure.results[0]?.reason).toBe(UI_LAYOUT_APPLY_REJECTION.invalidDocument);
    expect(parserFailure.results[0]?.reason).not.toContain(SESSION_ID);

    const membershipFailure = createHarness();
    membershipFailure.setApplyFailure(true);
    executeTargetedUiLayoutApply(applyFrame(), membershipFailure.dependencies);
    expect(membershipFailure.counts).toEqual({ decode: 1, apply: 1, mutations: 0 });
    expect(membershipFailure.events).toEqual(["decode", "apply", "ack"]);
    expect(membershipFailure.results[0]).toEqual({
      correlationId: "correlation-1",
      outcome: UiApplyLayoutOutcome.REJECTED,
      reason: UI_LAYOUT_APPLY_REJECTION.invalidDocument,
    });
  });

  test("ignores broadcast, wrong-tab, wrong-socket, and incomplete apply targets", () => {
    const frames = [
      applyFrame({ targetTabId: "" }),
      applyFrame({ targetTabId: "tab-other" }),
      applyFrame({ targetSocketId: "socket-old" }),
      applyFrame({ targetSocketId: "" }),
      applyFrame({ correlationId: "" }),
    ];
    for (const frame of frames) {
      const harness = createHarness();
      expect(executeTargetedUiLayoutApply(frame, harness.dependencies)).toBe(true);
      expect(harness.counts).toEqual({ decode: 0, apply: 0, mutations: 0 });
      expect(harness.events).toEqual([]);
      expect(harness.results).toEqual([]);
      expect(harness.settlements).toEqual([]);
    }
  });

  test("records local apply but does not ACK an identity replaced after commit", () => {
    for (const replaceIdentity of ["socket", "tab"] as const) {
      const harness = createHarness();
      harness.setIdentityReplacement(replaceIdentity);
      executeTargetedUiLayoutApply(applyFrame(), harness.dependencies);
      expect(harness.counts.mutations).toBe(1);
      expect(harness.events.at(-1)).toBe(`navigate:${SESSION_ID}`);
      expect(harness.results).toEqual([]);
      expect(harness.settlements).toEqual([{
        event: "ui_cc.layout_apply_settled",
        payload: { correlation_id: "correlation-1", outcome: "applied" },
      }]);
    }
  });

  test("a post-commit navigation throw still ACKs applied, never rejected", () => {
    const harness = createHarness();
    harness.setNavigationFailure(true);
    expect(() => executeTargetedUiLayoutApply(applyFrame(), harness.dependencies))
      .toThrow("router detail");
    expect(harness.counts.mutations).toBe(1);
    expect(harness.events).toEqual([
      "decode",
      "apply",
      "commit:worker::/work",
      "spotlight",
      `navigate:${SESSION_ID}`,
      "ack",
    ]);
    expect(harness.results[0]?.outcome).toBe(UiApplyLayoutOutcome.APPLIED);
    expect(harness.results[0]?.reason).toBeUndefined();
  });
});

describe("UI layout apply bridge and legacy separation", () => {
  test("an exact apply with no bridge receives the stable rejected ACK", () => {
    const harness = createHarness();
    expect(rejectTargetedUiLayoutApplyWithoutBridge(
      applyFrame(),
      harness.dependencies,
    )).toBe(true);
    expect(harness.results).toEqual([{
      correlationId: "correlation-1",
      outcome: UiApplyLayoutOutcome.REJECTED,
      reason: UI_LAYOUT_APPLY_REJECTION.bridgeUnavailable,
    }]);
    expect(harness.settlements).toEqual([{
      event: "ui_cc.layout_apply_settled",
      payload: { correlation_id: "correlation-1", outcome: "rejected" },
    }]);
  });

  test("a stale or wrong apply with no bridge receives no ACK", () => {
    for (const frame of [
      applyFrame({ targetTabId: "tab-other" }),
      applyFrame({ targetSocketId: "socket-old" }),
    ]) {
      const harness = createHarness();
      expect(rejectTargetedUiLayoutApplyWithoutBridge(frame, harness.dependencies)).toBe(true);
      expect(harness.results).toEqual([]);
      expect(harness.settlements).toEqual([]);
    }
  });

  test("all eight legacy commands remain outside execution acknowledgements", () => {
    const legacyCommands = [
      { case: "navigate", value: { path: "/" } },
      { case: "placeSplit", value: {} },
      { case: "selectTab", value: {} },
      { case: "focusPane", value: {} },
      { case: "moveTab", value: {} },
      { case: "arrange", value: {} },
      { case: "closeTab", value: {} },
      { case: "spotlight", value: {} },
    ];
    for (const command of legacyCommands) {
      const harness = createHarness();
      const frame = applyFrame({
        targetTabId: "",
        targetSocketId: "",
        correlationId: "",
        command: { command },
      } as unknown as Partial<UiCommandFrame>);
      expect(executeTargetedUiLayoutApply(frame, harness.dependencies)).toBe(false);
      expect(rejectTargetedUiLayoutApplyWithoutBridge(frame, harness.dependencies)).toBe(false);
      expect(harness.events).toEqual([]);
      expect(harness.results).toEqual([]);
    }
  });
});
