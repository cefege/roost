import { loadBundledAgents } from "@omp-source/packages/coding-agent/src/task/agents";
import type { BrowserAction, BuiltinCase, Checkpoint, Comparison } from "./scenario";
import { commandCase, composer, dismissThenSend, effect, literalPattern, observed, send, textComparison } from "./builtin-steps";

const down = "\u001b[B";
const right = "\u001b[C";
const taskIndex = loadBundledAgents().sort((left, right) => left.name.localeCompare(right.name)).findIndex(agent => agent.name === "task");
if (taskIndex < 0) throw new Error("Stock bundled task agent is required by the agents workflow");
const taskDescription = "General-purpose subagent with full capabilities for delegated multi-step tasks";
const small = "openai/parity-small";
const coder = "openai/parity-coder";
type AgentProperty = "model" | "prewalk" | "advisor";
const settingFor: Record<AgentProperty, string> = { model: "agentModelOverrides", prewalk: "agentPrewalk", advisor: "agentAdvisor" };

function presses(keys: string[]): BrowserAction[] {
  return keys.map(key => ({ type: "press", key }));
}

function navigate(name: string, terminalInput: string, keys: string[], comparisons: Comparison[]): Checkpoint {
  return { name, terminalInput, browserActions: presses(keys), comparisons };
}

function saved(property: AgentProperty, value?: string): Comparison {
  return effect("builtin.persisted-settings", value ?? false, `/task/${settingFor[property]}/${value === undefined ? "exists" : "task"}`);
}

function chooseTask(comparisons: Comparison[] = []): Checkpoint {
  return {
    name: "Select the real bundled task agent",
    terminalInput: down.repeat(taskIndex),
    browserActions: [{ type: "click", selector: '[data-omp-picker-item="agent:bundled:task"]' }],
    comparisons: [textComparison(taskDescription), ...comparisons],
  };
}

function propertyStrip(property: AgentProperty): Checkpoint {
  const moves = property === "model" ? 0 : property === "prewalk" ? 1 : 2;
  return navigate(`Configure task ${property}`, `\r${right.repeat(moves)}\r`, ["Enter", ...Array<string>(moves).fill("ArrowRight"), "Enter"], [textComparison(property === "model" ? "pick model…" : "agent default")]);
}

function patternDraft(property: AgentProperty, value: string): Checkpoint[] {
  const moves = property === "model" ? 1 : 4;
  return [
    propertyStrip(property),
    navigate(`Open task ${property} pattern editor`, `${right.repeat(moves)}\r`, [...Array<string>(moves).fill("ArrowRight"), "Enter"], [textComparison(`task ${property} pattern:`)]),
    {
      name: `Edit task ${property} pattern`, terminalInput: `\u0015${value}`,
      browserActions: [{ type: "fill", selector: "input:visible", value }],
      comparisons: [{ name: "Unsubmitted pattern draft", terminal: { type: "screen", pattern: `(${literalPattern(value)})`, group: 1 }, browser: { type: "dom", selector: "input:visible", read: "value" }, expected: value }],
    },
  ];
}

function commitPattern(property: AgentProperty, value: string): Checkpoint[] {
  const notice = property === "model" ? `task model: ${value} → ${value}` : `task ${property}: on (${value})`;
  return [...patternDraft(property, value), navigate(`Persist task ${property} pattern`, "\r", ["Enter"], [textComparison(notice), saved(property, value)])];
}

function modelBrowser(property: AgentProperty): Checkpoint[] {
  const moves = property === "model" ? 0 : 3;
  return [propertyStrip(property), navigate(`Browse actual models for task ${property}`, `${right.repeat(moves)}\r`, [...Array<string>(moves).fill("ArrowRight"), "Enter"], [textComparison(`Picking ${property === "model" ? "model override" : `${property} model`} for task`)])];
}

function chooseSmall(property: AgentProperty): Checkpoint[] {
  const notice = property === "model" ? `task model: ${small} → ${small}` : `task ${property}: on (${small})`;
  return [
    ...modelBrowser(property),
    {
      name: `Filter actual ${property} model browser`, terminalInput: "parity-small",
      browserActions: [{ type: "fill", selector: "input:visible", value: "parity-small" }],
      comparisons: [textComparison("openai/parity-small")],
    },
    navigate(`Commit actual ${property} model selection`, "\r", ["Enter"], [textComparison(notice), saved(property, small)]),
  ];
}

function clearProperty(property: AgentProperty): Checkpoint[] {
  const moves = property === "model" ? 2 : 0;
  return [propertyStrip(property), navigate(`Clear task ${property} override`, `${right.repeat(moves)}\r`, [...Array<string>(moves).fill("ArrowRight"), "Enter"], [textComparison(property === "model" ? `task model: @task → ${coder}` : `task ${property}: off`), saved(property)])];
}

const baseSettings = {
  "task.agentModelOverrides": {}, "task.agentPrewalk": {}, "task.agentAdvisor": {}, "task.disabledAgents": [],
  "task.prewalk": false, "advisor.enabled": false, "memory.backend": "off", "autolearn.enabled": false,
};

const configured = [saved("model", `${small}:high`), saved("prewalk", coder), saved("advisor", small)];

export const agentsWorkflowCases: BuiltinCase[] = [
  {
    ...commandCase("agents-workflow-configure-restart", "/agents", "Agents", [
      chooseTask(),
      ...chooseSmall("model"),
      ...commitPattern("model", `${small}:high`),
      ...commitPattern("prewalk", coder),
      ...chooseSmall("advisor"),
      navigate("Dismiss hub without reverting committed settings", "\u001b", ["Escape"], [
        { name: "Composer regained focus", terminal: { type: "screen", pattern: "(parity-coder)", group: 1 }, browser: { type: "dom", selector: "[data-omp-role='omp.composer.model']", read: "text", pattern: "(parity-coder)", group: 1 }, expected: "parity-coder" }, ...configured,
      ]),
      send("/agents", "Agents", configured),
      chooseTask([textComparison(`${small}:high`), textComparison(coder), ...configured]),
      dismissThenSend("/rename Agents workflow persisted", 'Session renamed to "Agents workflow persisted".', configured),
      send("/restart", "Agents workflow persisted", [effect("builtin.process", 1, "/restartCount"), ...configured]),
      send("/agents", "Agents", configured),
      chooseTask([textComparison(`${small}:high`), textComparison(coder), ...configured]),
      ...clearProperty("model"), ...clearProperty("prewalk"), ...clearProperty("advisor"),
      dismissThenSend("/agents", "Agents", [saved("model"), saved("prewalk"), saved("advisor")]),
      chooseTask([saved("model"), saved("prewalk"), saved("advisor")]),
    ]),
    settings: baseSettings,
    behaviors: ["agents:stock-hub", "agents:model-browser-commit", "agents:per-agent-thinking", "agents:prewalk-model", "agents:advisor-model", "agents:reopen", "agents:real-restart-persistence", "agents:clear-overrides"],
  },
  ...(["model", "prewalk", "advisor"] as const).map((property): BuiltinCase => ({
    ...commandCase(`agents-workflow-${property}-cancel`, "/agents", "Agents", [
      chooseTask(), ...chooseSmall(property),
      ...patternDraft(property, `${coder}:low`),
      navigate("Cancel unsaved pattern and return through both strips", "\u001b\u001b\u001b", ["Escape", "Escape", "Escape"], [textComparison(taskDescription), saved(property, small)]),
      ...modelBrowser(property),
      {
        name: "Filter a different model without committing", terminalInput: "parity-coder",
        browserActions: [{ type: "fill", selector: "input:visible", value: "parity-coder" }],
        comparisons: [textComparison(coder), saved(property, small)],
      },
      navigate("Cancel model browser without changing saved selection", "\u001b", ["Escape"], [textComparison(taskDescription), saved(property, small)]),
      dismissThenSend("/agents", "Agents", [saved(property, small)]),
      chooseTask([textComparison(small), saved(property, small)]),
    ]), settings: baseSettings,
    behaviors: ["agents:pattern-cancel", "agents:model-browser-cancel", `agents:${property}-cancel-preserves-committed-value`, "agents:reopen"],
  })),
  {
    ...commandCase("agents-workflow-enabled-reopen", "/agents", "Agents", [
      chooseTask(),
      navigate("Disable selected stock agent", " ", ["Space"], [textComparison("task disabled"), effect("builtin.persisted-settings", ["task"], "/task/disabledAgents")]),
      dismissThenSend("/agents", "Agents", [effect("builtin.persisted-settings", ["task"], "/task/disabledAgents")]),
      chooseTask(),
      navigate("Re-enable selected stock agent", " ", ["Space"], [textComparison("task enabled"), effect("builtin.persisted-settings", [], "/task/disabledAgents")]),
    ]), settings: baseSettings, behaviors: ["agents:enabled-persistence", "agents:reopen"],
  },
  {
    ...commandCase("tools-workflow-scroll", "/tools", "Available Tools", [
      navigate("Scroll stock tool report to the final write tool", "\u001b[F", ["End"], [
        { name: "Final real tool is visible", terminal: { type: "screen", pattern: "\\bwrite\\b", read: "visible" }, browser: { type: "dom", selector: "[data-omp-role='omp.report.body'] :text-is('write')", read: "insideViewport" }, expected: true },
      ]),
      navigate("Return to the first read tool", "\u001b[H", ["Home"], [
        { name: "First real tool is visible", terminal: { type: "screen", pattern: "\\bread\\b", read: "visible" }, browser: { type: "dom", selector: "[data-omp-role='omp.report.body'] :text-is('read')", read: "insideViewport" }, expected: true },
      ]),
      dismissThenSend("/tools", "Available Tools"),
    ]), settings: { ...baseSettings, "tools.xdev": false, "plan.enabled": false, "goal.enabled": false, "browser.enabled": false, "generate_image.enabled": false, "speechgen.enabled": false },
    behaviors: ["tools:actual-catalog", "tools:report-end-home", "tools:reopen"],
  },
  {
    ...commandCase("context-workflow-transcript-navigation", "/context", "User / Developer Messages", [
      navigate("Open actual seeded user message", `${down.repeat(2)}\r`, ["ArrowDown", "ArrowDown", "Enter"], [textComparison("PARITY_CONTEXT_USER_CONTENT")]),
      navigate("Back from user content to category overview", "\u001b", ["Escape"], [textComparison("User / Developer Messages")]),
      navigate("Select and open actual assistant message", `${down}\r`, ["ArrowDown", "Enter"], [textComparison("PARITY_CONTEXT_ASSISTANT_CONTENT")]),
      navigate("Back from assistant content to overview", "\u001b", ["Escape"], [textComparison("Assistant Messages")]),
      dismissThenSend("/context", "User / Developer Messages"),
      navigate("Reopened snapshot retains actual assistant message", `${down.repeat(3)}\r`, ["ArrowDown", "ArrowDown", "ArrowDown", "Enter"], [textComparison("PARITY_CONTEXT_ASSISTANT_CONTENT"), effect("builtin.session", 2, "/messageCount")]),
    ]), settings: { ...baseSettings, "tools.xdev": false }, behaviors: ["context:actual-session-snapshot", "context:user-assistant-content", "context:back", "context:reopen"],
  },
  {
    ...commandCase("hotkeys-workflow-copy-prompt", "/hotkeys", "Copy whole prompt", [
      {
        name: "Dismiss shortcuts and compose an unsent draft", terminalInput: "\u001b[27uParity shortcut draft",
        browserActions: [{ type: "press", key: "Escape" }, { type: "fill", selector: composer, value: "Parity shortcut draft" }],
        comparisons: [{ name: "Unsent composer draft", terminal: { type: "screen", pattern: "(Parity shortcut draft)", group: 1 }, browser: { type: "dom", selector: composer, read: "value" }, expected: "Parity shortcut draft" }],
      },
      navigate("Use the documented real copy-prompt shortcut", "\u001b[99;4u", ["Alt+Shift+C"], [textComparison("Copied: Parity shortcut draft"), effect("builtin.clipboard", "Parity shortcut draft"), effect("builtin.session", 0, "/messageCount")]),
    ]), settings: baseSettings, behaviors: ["hotkeys:documented-shortcut", "hotkeys:actual-host-clipboard", "hotkeys:unsent-draft-preserved"],
  },
];
