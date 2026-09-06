// UI-state output tests separate human terminal safety from lossless JSON.
// Adversarial protobuf strings cover controls, bidi formatting, and long text.
// Injected writers prove one remote value cannot create an output row.

import { create } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import type { LayoutDocumentV1 } from "@roost/shared/layout-document";
import { layoutDocumentToProto } from "@roost/shared/layout-document-proto";
import { UiTabStateSchema } from "@roost/shared/proto/coordinator_pb";
import { UiReportStateRequestSchema } from "@roost/shared/proto/sync_pb";
import { utf8ByteLength } from "@roost/shared/ui-state";
import {
  dispatchUiApi,
  type UiApiClient,
  type UiApiIo,
} from "../src/api-ui.ts";
import {
  TERMINAL_SAFE_TEXT_MAX_CODE_POINTS,
  TERMINAL_SAFE_TEXT_MAX_UTF8_BYTES,
  TERMINAL_SAFE_TEXT_TRUNCATION_MARKER,
} from "../src/terminal-safe-text.ts";

const TERMINAL_ATTACK = "\u001b]52;c;payload\u0007\n\t\r\u0000\u007f\u0085\u009b"
  + "\u061c\u200e\u202e\u2066\u2069\ufeff\u2028\u2029\\tail";
const SESSION_ID = `session${TERMINAL_ATTACK}${"\u001b".repeat(100)}`;
const LONG_LABEL = `${TERMINAL_ATTACK}${"a".repeat(400)}`;
const LONG_PATH = `${TERMINAL_ATTACK}${"🐙".repeat(200)}`;
const ATTACK_DOCUMENT = {
  schema_version: 1,
  root: {
    kind: "leaf",
    leaf_key: "leaf",
    slot_keys: ["slot"],
    selected_slot_key: "slot",
  },
  focused_leaf_key: "leaf",
  bindings: [{ slot_key: "slot", session_id: SESSION_ID }],
} as const satisfies LayoutDocumentV1;
const ATTACK_TAB = create(UiTabStateSchema, {
  fp: `${TERMINAL_ATTACK}fingerprint`,
  tabId: `tab${TERMINAL_ATTACK}`,
  label: LONG_LABEL,
  lastMs: 1_800_000_000_000n,
  state: create(UiReportStateRequestSchema, {
    tabId: `state${TERMINAL_ATTACK}`,
    activePath: LONG_PATH,
    folderKey: `folder${TERMINAL_ATTACK}`,
    layoutDocument: layoutDocumentToProto(ATTACK_DOCUMENT),
  }),
});

type CapturedOutput = {
  handled: boolean;
  output: string[];
  errors: string[];
};

async function captureUiState(args: readonly string[]): Promise<CapturedOutput> {
  const output: string[] = [];
  const errors: string[] = [];
  const client: UiApiClient = {
    async uiListStates() {
      return { tabs: [ATTACK_TAB] };
    },
    async uiDispatch() {
      throw new Error("ui-state must not dispatch a command");
    },
    async uiApplyLayout() {
      throw new Error("ui-state must not apply a layout");
    },
  };
  const io: UiApiIo = {
    writeLine: (line) => output.push(line),
    writeError: (line) => errors.push(line),
    setExitCode: () => {
      throw new Error("ui-state must not set an exit code");
    },
    readTextFile: async () => {
      throw new Error("ui-state must not read a file");
    },
    now: () => 1_800_000_042_000,
  };
  const handled = await dispatchUiApi(client, "ui-state", args, io);
  return { handled, output, errors };
}

describe("UI API human output safety", () => {
  test("visibly escapes terminal controls and bounds every displayed remote field", async () => {
    const result = await captureUiState([]);
    expect(result.handled).toBe(true);
    expect(result.errors).toEqual([]);
    expect(result.output).toHaveLength(2);

    const cells = result.output[0]!.split("\t");
    expect(cells).toHaveLength(6);
    const rendered = result.output.join("\n");
    for (const escape of [
      "\\x1b", "\\x07", "\\n", "\\t", "\\r", "\\x00", "\\x7f", "\\x85", "\\x9b",
      "\\u{61c}", "\\u{200e}", "\\u{202e}", "\\u{2066}", "\\u{2069}", "\\u{feff}",
      "\\u{2028}", "\\u{2029}", "\\\\tail",
    ]) expect(rendered).toContain(escape);

    const unsafeCharacter = /[\x00-\x08\x0a-\x1f\x7f-\x9f\p{Cf}\p{Zl}\p{Zp}]/u;
    for (const cell of cells) {
      expect(cell).not.toMatch(unsafeCharacter);
      expect([...cell].length).toBeLessThanOrEqual(TERMINAL_SAFE_TEXT_MAX_CODE_POINTS);
      expect(utf8ByteLength(cell)).toBeLessThanOrEqual(TERMINAL_SAFE_TEXT_MAX_UTF8_BYTES);
    }
    const treeLine = result.output[1]!;
    expect(treeLine).not.toMatch(unsafeCharacter);
    const displayedSessionId = treeLine.slice(treeLine.indexOf("[") + 1, -2);
    expect([...displayedSessionId].length).toBeLessThanOrEqual(TERMINAL_SAFE_TEXT_MAX_CODE_POINTS);
    expect(utf8ByteLength(displayedSessionId)).toBeLessThanOrEqual(TERMINAL_SAFE_TEXT_MAX_UTF8_BYTES);
    expect(displayedSessionId).toStartWith("session\\x1b");
    expect(displayedSessionId).toEndWith(TERMINAL_SAFE_TEXT_TRUNCATION_MARKER);
    expect(cells[2]).toEndWith(TERMINAL_SAFE_TEXT_TRUNCATION_MARKER);
    expect(cells[4]).toEndWith(TERMINAL_SAFE_TEXT_TRUNCATION_MARKER);
  });

  test("keeps the JSON projection byte-for-byte unformatted and untruncated", async () => {
    const result = await captureUiState(["--json"]);
    const expected = [{
      fp: ATTACK_TAB.fp,
      tab_id: ATTACK_TAB.tabId,
      label: LONG_LABEL,
      last_ms: 1_800_000_000_000,
      state: {
        tab_id: ATTACK_TAB.state!.tabId,
        active_path: LONG_PATH,
        folder_key: ATTACK_TAB.state!.folderKey,
        layout_document: ATTACK_DOCUMENT,
      },
    }];
    expect(result.output).toEqual([JSON.stringify(expected, null, 2)]);
    expect(JSON.parse(result.output[0]!)).toEqual(expected);
    expect(result.output[0]).not.toContain(TERMINAL_SAFE_TEXT_TRUNCATION_MARKER);
  });
});
