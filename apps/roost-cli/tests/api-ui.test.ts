// UI API CLI tests pin strict layout-file admission and acknowledged outcomes.
// Protobuf fixtures prove the ordinary public JSON and tree projections stay typed.
// Injected I/O proves malformed apply requests never read unexpectedly or reach RPCs.
// Legacy argv and hostile human output have focused sibling suites.

import { create } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import { layoutDocumentFromProto, layoutDocumentToProto } from "@roost/shared/layout-document-proto";
import type { LayoutDocumentV1 } from "@roost/shared/layout-document";
import { UiTabStateSchema } from "@roost/shared/proto/coordinator_pb";
import {
  UiApplyLayoutOutcome,
  UiReportStateRequestSchema,
} from "@roost/shared/proto/sync_pb";
import {
  dispatchUiApi,
  parseUiApplyLayoutArgs,
  prepareUiApplyLayout,
  type PreparedUiApplyLayout,
  type UiApiClient,
  type UiApiIo,
} from "../src/api-ui.ts";

const DOCUMENT = {
  schema_version: 1,
  root: {
    kind: "split",
    direction: "row",
    ratio: 0.5,
    first: {
      kind: "leaf",
      leaf_key: "leaf-left",
      slot_keys: ["slot-left"],
      selected_slot_key: "slot-left",
    },
    second: {
      kind: "leaf",
      leaf_key: "leaf-right",
      slot_keys: ["slot-right-a", "slot-right-b"],
      selected_slot_key: "slot-right-a",
    },
  },
  focused_leaf_key: "leaf-right",
  bindings: [
    { slot_key: "slot-left", session_id: "session-left" },
    { slot_key: "slot-right-a", session_id: "session-right-a" },
    { slot_key: "slot-right-b", session_id: "session-right-b" },
  ],
} as const satisfies LayoutDocumentV1;

const REPORTED_TAB = create(UiTabStateSchema, {
  fp: "0123456789abcdef",
  tabId: "tab-report",
  label: "Main browser",
  lastMs: 1_800_000_000_000n,
  state: create(UiReportStateRequestSchema, {
    tabId: "tab-report",
    activePath: "/s/session-right-a",
    folderKey: "worker::/repo",
    layoutDocument: layoutDocumentToProto(DOCUMENT),
  }),
});

const OFF_TERMINAL_TAB = create(UiTabStateSchema, {
  fp: "fedcba9876543210",
  tabId: "tab-home",
  label: "",
  lastMs: 1_800_000_000_010n,
  state: create(UiReportStateRequestSchema, {
    tabId: "tab-home",
    activePath: "/",
    folderKey: "",
  }),
});
function applyTargetTab(fingerprint: string) {
  return create(UiTabStateSchema, {
    fp: fingerprint,
    tabId: "tab-target",
    label: "",
    lastMs: 1_800_000_000_020n,
  });
}


interface FakeOptions {
  tabs?: typeof REPORTED_TAB[];
  delivered?: number;
  outcome?: UiApplyLayoutOutcome;
  reason?: string;
}

type CallRecord = {
  method: "list" | "dispatch" | "apply";
  request: unknown;
};

function fakeClient(options: FakeOptions = {}): {
  client: UiApiClient;
  calls: CallRecord[];
} {
  const calls: CallRecord[] = [];
  return {
    calls,
    client: {
      async uiListStates(request) {
        calls.push({ method: "list", request });
        return { tabs: options.tabs ?? [] };
      },
      async uiDispatch(request) {
        calls.push({ method: "dispatch", request });
        return { delivered: options.delivered ?? 2 };
      },
      async uiApplyLayout(request) {
        calls.push({ method: "apply", request });
        return {
          outcome: options.outcome ?? UiApplyLayoutOutcome.APPLIED,
          reason: options.reason,
        };
      },
    },
  };
}

interface Invocation {
  output: string[];
  errors: string[];
  exits: number[];
  readPaths: string[];
  promise: Promise<boolean>;
}

function invoke(
  client: UiApiClient,
  verb: string,
  args: readonly string[],
  fileResult: string | Error = JSON.stringify(DOCUMENT),
  preparedApplyLayout?: PreparedUiApplyLayout,
): Invocation {
  const output: string[] = [];
  const errors: string[] = [];
  const exits: number[] = [];
  const readPaths: string[] = [];
  const io: UiApiIo = {
    writeLine: (line) => output.push(line),
    writeError: (line) => errors.push(line),
    setExitCode: (code) => exits.push(code),
    async readTextFile(path) {
      readPaths.push(path);
      if (fileResult instanceof Error) throw fileResult;
      return fileResult;
    },
    now: () => 1_800_000_042_000,
  };
  return {
    output,
    errors,
    exits,
    readPaths,
    promise: dispatchUiApi(client, verb, args, io, preparedApplyLayout),
  };
}

describe("UI API CLI", () => {
  test("leaves unrelated API verbs untouched", async () => {
    const { client, calls } = fakeClient();
    const invocation = invoke(client, "sessions", []);
    await expect(invocation.promise).resolves.toBe(false);
    expect(calls).toEqual([]);
    expect(invocation.output).toEqual([]);
  });

  test("prints an explicit snake_case typed ui-state JSON projection", async () => {
    const { client, calls } = fakeClient({ tabs: [REPORTED_TAB, OFF_TERMINAL_TAB] });
    const invocation = invoke(client, "ui-state", ["--json"]);
    await expect(invocation.promise).resolves.toBe(true);

    expect(JSON.parse(invocation.output[0]!)).toEqual([
      {
        fp: "0123456789abcdef",
        tab_id: "tab-report",
        label: "Main browser",
        last_ms: 1_800_000_000_000,
        state: {
          tab_id: "tab-report",
          active_path: "/s/session-right-a",
          folder_key: "worker::/repo",
          layout_document: DOCUMENT,
        },
      },
      {
        fp: "fedcba9876543210",
        tab_id: "tab-home",
        label: "",
        last_ms: 1_800_000_000_010,
        state: {
          tab_id: "tab-home",
          active_path: "/",
          folder_key: "",
          layout_document: null,
        },
      },
    ]);
    expect(invocation.output[0]).not.toMatch(/tabId|lastMs|activePath|folderKey|layoutDocument|\$typeName/);
    expect(calls).toEqual([{ method: "list", request: {} }]);
  });

  test("prints the typed document tree through slot bindings and portable focus", async () => {
    const { client } = fakeClient({ tabs: [REPORTED_TAB] });
    const invocation = invoke(client, "ui-state", []);
    await expect(invocation.promise).resolves.toBe(true);
    expect(invocation.output).toEqual([
      "01234567\ttab-report\tMain browser\t42s\t/s/session-right-a\tworker::/repo",
      "  split row 0.50",
      "    leaf [session-left*]",
      "    leaf (focused) [session-right-a*,session-right-b]",
    ]);
    expect(invocation.output.join("\n")).not.toMatch(/leaf-left|leaf-right|slot-|pane/i);
  });

  test("rejects unknown or duplicate ui-state flags before listing", async () => {
    for (const args of [["--raw"], ["--json", "--json"]]) {
      const { client, calls } = fakeClient({ tabs: [REPORTED_TAB] });
      const invocation = invoke(client, "ui-state", args);
      await expect(invocation.promise).rejects.toThrow(
        "ui-state: expected only one optional --json",
      );
      expect(calls).toEqual([]);
    }
  });

  test("parses exactly one file and one nonempty tab option", () => {
    expect(parseUiApplyLayoutArgs(["layout.json", "--tab", "tab-1"])).toEqual({
      filePath: "layout.json",
      targetTabId: "tab-1",
    });
    expect(parseUiApplyLayoutArgs(["--tab=tab-2", "layout.json"])).toEqual({
      filePath: "layout.json",
      targetTabId: "tab-2",
    });
  });

  test("reuses one prepared file without reading again during dispatch", async () => {
    const preflightReads: string[] = [];
    const prepared = await prepareUiApplyLayout(
      ["layout.json", "--tab", "tab-target"],
      async (path) => {
        preflightReads.push(path);
        return JSON.stringify(DOCUMENT);
      },
    );
    const { client, calls } = fakeClient({ tabs: [applyTargetTab("target-fingerprint")] });
    const invocation = invoke(client, "ui", [
      "apply-layout", "layout.json", "--tab", "tab-target",
    ], new Error("dispatch must not reread"), prepared);
    await expect(invocation.promise).resolves.toBe(true);
    expect(preflightReads).toEqual(["layout.json"]);
    expect(invocation.readPaths).toEqual([]);
    expect(invocation.output).toEqual(["applied"]);
    expect(calls.map((call) => call.method)).toEqual(["list", "apply"]);
    const request = calls[1]?.request as Parameters<UiApiClient["uiApplyLayout"]>[0];
    expect(request.targetFingerprint).toBe("target-fingerprint");
  });

  test("rejects missing, empty, duplicate, unknown, and extra arguments before I/O or RPC", async () => {
    const invalidArguments = [
      [],
      ["layout.json"],
      ["--tab", "tab-1"],
      ["layout.json", "--tab"],
      ["layout.json", "--tab", ""],
      ["layout.json", "--tab", "   "],
      ["layout.json", "--tab="],
      ["layout.json", "--tab", "tab-1", "--tab=tab-2"],
      ["layout.json", "--tab", "tab-1", "--unknown"],
      ["layout.json", "second.json", "--tab", "tab-1"],
    ];
    for (const args of invalidArguments) {
      const { client, calls } = fakeClient();
      const invocation = invoke(client, "ui", ["apply-layout", ...args]);
      await expect(invocation.promise).rejects.toThrow("ui apply-layout:");
      expect(invocation.readPaths).toEqual([]);
      expect(calls).toEqual([]);
    }
  });

  test("rejects missing, malformed, and non-snake_case files before RPC", async () => {
    const missing = fakeClient();
    const missingInvocation = invoke(missing.client, "ui", [
      "apply-layout", "missing.json", "--tab", "tab-1",
    ], new Error("ENOENT"));
    await expect(missingInvocation.promise).rejects.toThrow("cannot read \"missing.json\": ENOENT");
    expect(missingInvocation.readPaths).toEqual(["missing.json"]);
    expect(missing.calls).toEqual([]);

    for (const source of [
      "{not-json",
      JSON.stringify({ ...DOCUMENT, schema_version: 2 }),
      JSON.stringify({ schemaVersion: 1, root: DOCUMENT.root, focusedLeafKey: "leaf-right", bindings: [] }),
    ]) {
      const malformed = fakeClient();
      const invocation = invoke(malformed.client, "ui", [
        "apply-layout", "layout.json", "--tab", "tab-1",
      ], source);
      await expect(invocation.promise).rejects.toThrow("ui apply-layout:");
      expect(malformed.calls).toEqual([]);
    }
  });

  test("stops before apply when the reported target is absent or ambiguous", async () => {
    const absent = fakeClient({ tabs: [] });
    const absentInvocation = invoke(absent.client, "ui", [
      "apply-layout", "layout.json", "--tab", "tab-target",
    ]);
    await expect(absentInvocation.promise).resolves.toBe(true);
    expect(absentInvocation.output).toEqual(["target-gone"]);
    expect(absentInvocation.errors).toEqual(["target acknowledgement unavailable"]);
    expect(absentInvocation.exits).toEqual([1]);
    expect(absent.calls.map((call) => call.method)).toEqual(["list"]);

    const ambiguous = fakeClient({
      tabs: [applyTargetTab("fingerprint-a"), applyTargetTab("fingerprint-b")],
    });
    const ambiguousInvocation = invoke(ambiguous.client, "ui", [
      "apply-layout", "layout.json", "--tab", "tab-target",
    ]);
    await expect(ambiguousInvocation.promise).resolves.toBe(true);
    expect(ambiguousInvocation.output).toEqual(["rejected"]);
    expect(ambiguousInvocation.errors).toEqual(["target tab is ambiguous"]);
    expect(ambiguousInvocation.exits).toEqual([1]);
    expect(ambiguous.calls.map((call) => call.method)).toEqual(["list"]);
  });

  test("calls acknowledged apply once and prints exact outcomes with nonzero failures", async () => {
    for (const [outcome, label, exits] of [
      [UiApplyLayoutOutcome.APPLIED, "applied", []],
      [UiApplyLayoutOutcome.REJECTED, "rejected", [1]],
      [UiApplyLayoutOutcome.TARGET_GONE, "target-gone", [1]],
    ] as const) {
      const { client, calls } = fakeClient({
        tabs: [applyTargetTab("target-fingerprint")],
        outcome,
        reason: `${label} reason`,
      });
      const invocation = invoke(client, "ui", [
        "apply-layout", "layout.json", "--tab", "tab-target",
      ]);
      await expect(invocation.promise).resolves.toBe(true);
      expect(invocation.output).toEqual([label]);
      expect(invocation.errors).toEqual([`${label} reason`]);
      expect(invocation.exits).toEqual([...exits]);
      expect(invocation.readPaths).toEqual(["layout.json"]);
      expect(calls.map((call) => call.method)).toEqual(["list", "apply"]);
      const request = calls[1]?.request as Parameters<UiApiClient["uiApplyLayout"]>[0];
      expect(request.targetTabId).toBe("tab-target");
      expect(request.targetFingerprint).toBe("target-fingerprint");
      expect(layoutDocumentFromProto(request.document)).toEqual(DOCUMENT);
    }
  });

  test("refuses unspecified or unknown apply outcomes without printing success", async () => {
    for (const outcome of [
      UiApplyLayoutOutcome.UNSPECIFIED,
      99 as UiApplyLayoutOutcome,
    ]) {
      const { client, calls } = fakeClient({
        tabs: [applyTargetTab("target-fingerprint")],
        outcome,
      });
      const invocation = invoke(client, "ui", [
        "apply-layout", "layout.json", "--tab", "tab-target",
      ]);
      await expect(invocation.promise).rejects.toThrow(
        "ui apply-layout: coordinator returned an invalid outcome",
      );
      expect(invocation.output).toEqual([]);
      expect(invocation.exits).toEqual([]);
      expect(calls.map((call) => call.method)).toEqual(["list", "apply"]);
    }
  });

});
