// Owns `roost api ui-state` and the live-browser UI command family.
// Layout files and reported state cross one strict snake_case document boundary.
// The apply path prints one exact outcome; legacy commands keep
// their Sync publication-count contract. Called by api.ts.

import { readFile } from "node:fs/promises";
import {
  layoutDocumentFromProto,
  layoutDocumentToProto,
} from "@roost/shared/layout-document-proto";
import {
  parseLayoutDocumentV1,
  type LayoutDocumentNode,
  type LayoutDocumentV1,
} from "@roost/shared/layout-document";
import type { UiTabState } from "@roost/shared/proto/coordinator_pb";
import {
  UiApplyLayoutOutcome,
  type LayoutDocumentV1 as ProtoLayoutDocumentV1,
} from "@roost/shared/proto/sync_pb";
import { parseLegacyUiArgs, type LegacyUiCommand } from "./api-ui-legacy.ts";
import { formatTerminalSafeText } from "./terminal-safe-text.ts";

export interface UiApiClient {
  uiListStates(request: Record<string, never>): Promise<{ tabs: UiTabState[] }>;
  uiDispatch(request: {
    targetTabId: string;
    command: LegacyUiCommand;
  }): Promise<{ delivered: number }>;
  uiApplyLayout(request: {
    targetTabId: string;
    targetFingerprint: string;
    document: ProtoLayoutDocumentV1;
  }): Promise<{
    outcome: UiApplyLayoutOutcome;
    reason?: string;
  }>;
}

export interface UiApiIo {
  writeLine(line: string): void;
  writeError(line: string): void;
  setExitCode(code: number): void;
  readTextFile(path: string): Promise<string>;
  now(): number;
}

export interface UiApplyLayoutArgs {
  filePath: string;
  targetTabId: string;
}

export interface PreparedUiApplyLayout {
  targetTabId: string;
  document: ProtoLayoutDocumentV1;
}

type PublicUiTabState = {
  fp: string;
  tab_id: string;
  label: string;
  last_ms: number;
  state: {
    tab_id: string;
    active_path: string;
    folder_key: string;
    layout_document: LayoutDocumentV1 | null;
  } | null;
};

const UI_USAGE =
  "roost api ui <cmd> [--tab <tabId>]: navigate <path> | place-split <sid> <anchorSid> <row|col> [--first] | "
  + "select-tab <sid> | focus-pane <sid> | move-tab <sid> <destSid> | "
  + "arrange <even|rows|tiled|main-vertical|balance> | close-tab <sid> | spotlight <sid> [--off] | "
  + "apply-layout <file> --tab <tabId>";
const UI_LAYOUT_TARGET_GONE_REASON = "target acknowledgement unavailable";
const UI_LAYOUT_AMBIGUOUS_REASON = "target tab is ambiguous";


const DEFAULT_IO: UiApiIo = {
  writeLine: (line) => console.log(line),
  writeError: (line) => console.error(line),
  setExitCode: (code) => {
    process.exitCode = code;
  },
  readTextFile: (path) => readFile(path, "utf8"),
  now: () => Date.now(),
};

export async function prepareUiApplyLayout(
  args: readonly string[],
  readTextFile: (path: string) => Promise<string> = DEFAULT_IO.readTextFile,
): Promise<PreparedUiApplyLayout> {
  const parsed = parseUiApplyLayoutArgs(args);
  return { targetTabId: parsed.targetTabId, document: await readLayoutDocument(parsed.filePath, readTextFile) };
}

export async function dispatchUiApi(
  client: UiApiClient,
  verb: string,
  args: readonly string[],
  overrides: Partial<UiApiIo> = {},
  preparedApplyLayout?: PreparedUiApplyLayout,
): Promise<boolean> {
  if (verb !== "ui" && verb !== "ui-state") return false;
  const io = { ...DEFAULT_IO, ...overrides };

  if (verb === "ui-state") {
    await printUiState(client, args, io);
    return true;
  }

  if (args[0] === "apply-layout") {
    const prepared = preparedApplyLayout ?? await prepareUiApplyLayout(args.slice(1), io.readTextFile);
    await applyLayout(client, prepared, io);
    return true;
  }

  const parsed = parseLegacyUiArgs(args);
  if (parsed === null) {
    io.writeError(UI_USAGE);
    io.setExitCode(1);
    return true;
  }
  const { subcommand, targetTabId, positionals } = parsed;
  switch (subcommand) {
    case "navigate":
      await publishLegacyCommand(client, targetTabId, {
        command: { case: "navigate", value: { path: positionals[0]! } },
      }, io);
      break;
    case "place-split": {
      const direction = positionals[2]!;
      if (direction !== "row" && direction !== "col") {
        io.writeError(`roost api: dir must be row|col, got "${direction}"`);
        io.setExitCode(1);
        return true;
      }
      await publishLegacyCommand(client, targetTabId, {
        command: { case: "placeSplit", value: {
          sessionId: positionals[0]!,
          anchorSessionId: positionals[1]!,
          dir: direction,
          insertFirst: parsed.insertFirst,
        } },
      }, io);
      break;
    }
    case "select-tab":
      await publishLegacyCommand(client, targetTabId, {
        command: { case: "selectTab", value: { sessionId: positionals[0]! } },
      }, io);
      break;
    case "focus-pane":
      await publishLegacyCommand(client, targetTabId, {
        command: { case: "focusPane", value: { sessionId: positionals[0]! } },
      }, io);
      break;
    case "move-tab":
      await publishLegacyCommand(client, targetTabId, {
        command: { case: "moveTab", value: {
          sessionId: positionals[0]!,
          destSessionId: positionals[1]!,
        } },
      }, io);
      break;
    case "arrange": {
      const preset = positionals[0]!;
      if (!["even", "rows", "tiled", "main-vertical", "balance"].includes(preset)) {
        io.writeError(`roost api: preset must be even|rows|tiled|main-vertical|balance, got "${preset}"`);
        io.setExitCode(1);
        return true;
      }
      await publishLegacyCommand(client, targetTabId, {
        command: { case: "arrange", value: { preset } },
      }, io);
      break;
    }
    case "close-tab":
      await publishLegacyCommand(client, targetTabId, {
        command: { case: "closeTab", value: { sessionId: positionals[0]! } },
      }, io);
      break;
    case "spotlight":
      await publishLegacyCommand(client, targetTabId, {
        command: { case: "spotlight", value: {
          sessionId: positionals[0]!,
          off: parsed.spotlightOff,
        } },
      }, io);
      break;
  }
  return true;
}

export function parseUiApplyLayoutArgs(args: readonly string[]): UiApplyLayoutArgs {
  let filePath: string | undefined;
  let targetTabId: string | undefined;

  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index]!;
    if (argument === "--tab") {
      if (targetTabId !== undefined) throw new Error("ui apply-layout: duplicate --tab");
      const value = args[index + 1];
      if (!value || value.startsWith("--") || value.trim().length === 0) {
        throw new Error("ui apply-layout: --tab requires a nonempty value");
      }
      targetTabId = value;
      index += 1;
      continue;
    }
    if (argument.startsWith("--tab=")) {
      if (targetTabId !== undefined) throw new Error("ui apply-layout: duplicate --tab");
      const value = argument.slice("--tab=".length);
      if (value.trim().length === 0) {
        throw new Error("ui apply-layout: --tab requires a nonempty value");
      }
      targetTabId = value;
      continue;
    }
    if (argument.startsWith("--")) {
      throw new Error(`ui apply-layout: unknown option ${JSON.stringify(argument)}`);
    }
    if (filePath !== undefined) {
      throw new Error("ui apply-layout: expected exactly one <file>");
    }
    if (argument.length === 0) throw new Error("ui apply-layout: <file> must be nonempty");
    filePath = argument;
  }

  if (filePath === undefined) throw new Error("ui apply-layout: missing <file>");
  if (targetTabId === undefined) throw new Error("ui apply-layout: missing --tab <id>");
  return { filePath, targetTabId };
}

async function printUiState(
  client: UiApiClient,
  args: readonly string[],
  io: UiApiIo,
): Promise<void> {
  if (args.some((argument) => argument !== "--json") || args.filter((argument) => argument === "--json").length > 1) {
    throw new Error("ui-state: expected only one optional --json");
  }
  const { tabs } = await client.uiListStates({});
  const projected = tabs.map(projectUiTabState);
  if (args[0] === "--json") {
    io.writeLine(JSON.stringify(projected, null, 2));
    return;
  }
  const now = io.now();
  for (const tab of projected) {
    io.writeLine([
      formatTerminalSafeText(tab.fp.slice(0, 8)),
      formatTerminalSafeText(tab.tab_id),
      formatTerminalSafeText(tab.label || "-"),
      humanAge(now - tab.last_ms),
      formatTerminalSafeText(tab.state?.active_path ?? ""),
      formatTerminalSafeText(tab.state?.folder_key ?? ""),
    ].join("\t"));
    if (tab.state?.layout_document) printLayoutTree(tab.state.layout_document, io.writeLine);
  }
}

function projectUiTabState(tab: UiTabState): PublicUiTabState {
  const lastMs = Number(tab.lastMs);
  if (!Number.isSafeInteger(lastMs) || lastMs < 0) {
    throw new Error("ui-state: last_ms exceeds the JSON integer range");
  }
  return {
    fp: tab.fp,
    tab_id: tab.tabId,
    label: tab.label,
    last_ms: lastMs,
    state: tab.state
      ? {
          tab_id: tab.state.tabId,
          active_path: tab.state.activePath,
          folder_key: tab.state.folderKey,
          layout_document: tab.state.layoutDocument
            ? layoutDocumentFromProto(tab.state.layoutDocument)
            : null,
        }
      : null,
  };
}

function printLayoutTree(document: LayoutDocumentV1, writeLine: (line: string) => void): void {
  const sessionBySlot = new Map(
    document.bindings.map((binding) => [binding.slot_key, binding.session_id]),
  );
  const walk = (node: LayoutDocumentNode, indent: string): void => {
    if (node.kind === "split") {
      writeLine(`${indent}split ${node.direction} ${node.ratio.toFixed(2)}`);
      walk(node.first, `${indent}  `);
      walk(node.second, `${indent}  `);
      return;
    }
    const sessions = node.slot_keys.map((slotKey) => {
      const sessionId = sessionBySlot.get(slotKey);
      if (sessionId === undefined) {
        throw new Error(`ui-state: missing binding for slot ${formatTerminalSafeText(slotKey)}`);
      }
      const displayedSessionId = formatTerminalSafeText(sessionId);
      return slotKey === node.selected_slot_key ? `${displayedSessionId}*` : displayedSessionId;
    });
    const focused = node.leaf_key === document.focused_leaf_key ? " (focused)" : "";
    writeLine(`${indent}leaf${focused} [${sessions.join(",")}]`);
  };
  walk(document.root, "  ");
}

async function applyLayout(
  client: UiApiClient,
  prepared: PreparedUiApplyLayout,
  io: UiApiIo,
): Promise<void> {
  const { tabs } = await client.uiListStates({});
  let targetFingerprint: string | undefined;
  for (const tab of tabs) {
    if (tab.tabId !== prepared.targetTabId || !tab.fp) continue;
    if (targetFingerprint !== undefined && targetFingerprint !== tab.fp) {
      writeApplyOutcome(io, UiApplyLayoutOutcome.REJECTED, UI_LAYOUT_AMBIGUOUS_REASON);
      return;
    }
    targetFingerprint = tab.fp;
  }
  if (targetFingerprint === undefined) {
    writeApplyOutcome(io, UiApplyLayoutOutcome.TARGET_GONE, UI_LAYOUT_TARGET_GONE_REASON);
    return;
  }
  const response = await client.uiApplyLayout({ ...prepared, targetFingerprint });
  writeApplyOutcome(io, response.outcome, response.reason);
}

function writeApplyOutcome(
  io: UiApiIo,
  outcome: UiApplyLayoutOutcome,
  reason?: string,
): void {
  io.writeLine(applyOutcomeLabel(outcome));
  if (reason) io.writeError(reason);
  if (outcome !== UiApplyLayoutOutcome.APPLIED) io.setExitCode(1);
}

async function readLayoutDocument(
  filePath: string,
  readTextFile: (path: string) => Promise<string>,
): Promise<ProtoLayoutDocumentV1> {
  let source: string;
  try {
    source = await readTextFile(filePath);
  } catch (error) {
    throw new Error(`ui apply-layout: cannot read ${JSON.stringify(filePath)}: ${errorMessage(error)}`);
  }
  let decoded: unknown;
  try {
    decoded = JSON.parse(source);
  } catch {
    throw new Error(`ui apply-layout: ${JSON.stringify(filePath)} is not valid JSON`);
  }
  try {
    return layoutDocumentToProto(parseLayoutDocumentV1(decoded));
  } catch (error) {
    throw new Error(`ui apply-layout: ${JSON.stringify(filePath)} is not a valid layout document: ${errorMessage(error)}`);
  }
}

async function publishLegacyCommand(
  client: UiApiClient,
  targetTabId: string,
  command: LegacyUiCommand,
  io: UiApiIo,
): Promise<void> {
  const response = await client.uiDispatch({ targetTabId, command });
  io.writeLine(`delivered=${response.delivered}`);
  if (response.delivered === 0) {
    io.writeError("roost api: delivered=0 — no browser tab connected to coord; spatial commands need a live SPA (check `roost api ui-state`)");
  }
}

function applyOutcomeLabel(outcome: UiApplyLayoutOutcome): "applied" | "rejected" | "target_gone" {
  if (outcome === UiApplyLayoutOutcome.APPLIED) return "applied";
  if (outcome === UiApplyLayoutOutcome.REJECTED) return "rejected";
  if (outcome === UiApplyLayoutOutcome.TARGET_GONE) return "target_gone";
  throw new Error("ui apply-layout: coordinator returned an invalid outcome");
}

function humanAge(milliseconds: number): string {
  const seconds = Math.max(0, Math.round(milliseconds / 1_000));
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3_600) return `${Math.round(seconds / 60)}m`;
  return `${Math.round(seconds / 3_600)}h`;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
