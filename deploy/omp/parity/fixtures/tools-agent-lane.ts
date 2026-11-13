import type * as Cleanse from "@omp-source/packages/tui/src/overlays/cleanse-panel";
import type * as Board from "@omp-source/packages/tui/src/apps/cleanse-board";
import type * as Task from "@omp-source/packages/tui/src/tools/task";
import { panelControls } from "./panel-dialogs";
import type { Comparison, FixtureCase } from "../scenario";

const PANEL = "component:packages/tui/src/overlays/cleanse-panel.ts#CleansePanelComponent";
const LANE = '[data-omp-role="omp.cleanse.lane"]';
const SHEET_HEAD = '[data-omp-role="omp.sheet.head"]';
const TOOL_ACTION = "Report agent tool progress";
const RETRY_ACTION = "Report agent retry";
const FAR_FUTURE_MS = 1e12;

function agentProgress(overrides: Partial<Task.AgentProgress>): Task.AgentProgress {
  return {
    index: 0, id: "fixture-one", agent: "cleanse", agentSource: "bundled", status: "running", task: "Repair alpha.ts",
    recentTools: [], recentOutput: [], toolCount: 3, requests: 2, tokens: 450, cost: 0.05, durationMs: 0,
    resolvedModel: "fixture/lane-model", lastIntent: "Reviewing alpha.ts", ...overrides,
  } as Task.AgentProgress;
}

function laneComparison(name: string, terminalPattern: string, browserSelector: string, browserPattern: string, expected: string | number, entries: string[]): Comparison {
  return {
    name,
    terminal: { type: "screen", pattern: terminalPattern, group: 1 },
    browser: { type: "dom", selector: browserSelector, read: "text", pattern: browserPattern, group: 1 },
    expected, normalize: typeof expected === "number" ? "number" : "whitespace",
    proves: entries.map(entry => ({ entry, effect: "literal" as const, value: expected })),
  };
}

const idleLane: Comparison = {
  name: "lane row sits below the panel title before any progress",
  terminal: { type: "screen", pattern: "(fixture-one)", group: 1, relativeTo: { pattern: "Fixture lane progress" }, read: "below" },
  browser: { type: "dom", selector: LANE, relativeTo: SHEET_HEAD, read: "below" },
  expected: true,
  proves: [
    { entry: "property:TspAgentProps.tool", effect: "layout", value: null },
    { entry: "property:TspAgentProps.retry", effect: "layout", value: null },
  ],
};

function laneCase(id: string, action: string, progress: Partial<Task.AgentProgress>, covers: string[], comparisons: Comparison[]): FixtureCase {
  return {
    id, covers: [PANEL, ...covers],
    async create(context) {
      const { CleansePanelComponent } = await context.load<typeof Cleanse>("packages/tui/src/overlays/cleanse-panel.ts");
      const { CleanseBoardModel } = await context.load<typeof Board>("packages/tui/src/apps/cleanse-board.ts");
      const assignment: Board.CleanseAssignment = { index: 0, groups: [{ file: "alpha.ts" }], weight: 1 };
      const panel = new CleansePanelComponent({ model: new CleanseBoardModel(), request: "Fixture lane progress", tui: context.tui });
      panel.agentStarted("fixture-one", assignment);
      return panelControls(context, panel, { [action]: () => { panel.agentProgress("fixture-one", agentProgress(progress)); context.record("progress", action); } });
    },
    checkpoints: [
      { name: "started lane without progress", comparisons: [idleLane] },
      { name: "reported lane progress", terminalInput: "\r", browserActions: [{ type: "click", selector: `text="${action}"` }], comparisons },
    ],
  };
}

export const fixtureCases: FixtureCase[] = [
  laneCase("tool-agent-lane-tool", TOOL_ACTION, { currentTool: "read", currentToolArgs: "src/a.ts" },
    ["property:TspAgentProps.tool", "property:TspAgentProps.retry", "property:TspAgentProps.tool.name", "property:TspAgentProps.tool.intent", "property:TspAgentProps.stats.tools", "property:TspAgentProps.stats.tokens", "property:TspAgentProps.stats.cost"],
    [
      laneComparison("running tool name", "Reviewing alpha\\.ts (read) src/a\\.ts", `${LANE} p:text-matches("^Running")`, "Running (read)", "read", ["property:TspAgentProps.tool.name"]),
      laneComparison("running tool intent", "(Reviewing alpha\\.ts) read src/a\\.ts", `${LANE} p:text-matches("^Running")`, "Running read: (Reviewing alpha\\.ts)", "Reviewing alpha.ts", ["property:TspAgentProps.tool.intent"]),
      laneComparison("lane tool count", "(\\d+) tools", `${LANE} p:has-text("tools")`, "(\\d+) tools", 3, ["property:TspAgentProps.stats.tools"]),
      laneComparison("lane token count", "(\\d+) tok", `${LANE} p:has-text("tok")`, "(\\d+) tok", 450, ["property:TspAgentProps.stats.tokens"]),
      laneComparison("lane cost", "\\$(\\d+\\.\\d+)", `${LANE} p:has-text("tok")`, "\\$(\\d+\\.\\d+)", 0.05, ["property:TspAgentProps.stats.cost"]),
    ]),
  laneCase("tool-agent-lane-retry", RETRY_ACTION, { retryState: { attempt: 2, maxAttempts: 5, delayMs: 4000, startedAtMs: Date.now() + FAR_FUTURE_MS, errorMessage: "429 rate limit" } as Task.AgentProgress["retryState"] },
    ["property:TspAgentProps.retry", "property:TspAgentProps.retry.attempt", "property:TspAgentProps.retry.max"],
    [
      laneComparison("retry attempt", "retry (\\d+)/5", `${LANE} p:has-text("Retry")`, "Retry (\\d+)/5", 2, ["property:TspAgentProps.retry.attempt"]),
      laneComparison("retry limit", "retry 2/(\\d+)", `${LANE} p:has-text("Retry")`, "Retry 2/(\\d+)", 5, ["property:TspAgentProps.retry.max"]),
    ]),
];
