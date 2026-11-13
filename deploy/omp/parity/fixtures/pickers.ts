import { unlink, writeFile } from "node:fs/promises";
import { existsSync, writeFileSync } from "node:fs";
import type * as CatalogBuild from "@omp-source/packages/catalog/src/build";
import type * as ModelPicker from "@omp-source/packages/tui/src/overlays/model-picker";
import type { ModelBrowserSource } from "@omp-source/packages/tui/src/overlays/model-browser";
import type * as HistorySearch from "@omp-source/packages/tui/src/overlays/history-search";
import type * as SessionSelector from "@omp-source/packages/tui/src/overlays/session-selector";
import type * as TreeSelector from "@omp-source/packages/tui/src/overlays/tree-selector";
import type * as QueueModeSelector from "@omp-source/packages/tui/src/overlays/queue-mode-selector";
import { callbackValue, visibleText, type Comparison, type FixtureCase } from "../scenario";
import { fixtureCases as agentCases } from "./pickers-agents";
import { fixtureCases as modelCases } from "./pickers-models";
import { fixtureCases as hubCases } from "./pickers-hub";
import { fixtureCases as treeCases } from "./pickers-tree";
import { fixtureCases as accountCases } from "./pickers-accounts";
import { fixtureCases as sessionCases } from "./pickers-sessions";

const source = "packages/tui/src/overlays/";
const component = (file: string, name: string) => `component:${source}${file}.ts#${name}`;
// OMP labels a tree user turn "user: <text>"; both legs read that one label run.
function treeUserPrompt(name: string, id: string, prompt: string): Comparison {
  return {
    name,
    terminal: { type: "screen", pattern: `user: (${prompt})`, group: 1 },
    browser: { type: "dom", selector: `[data-omp-picker-item="${id}"] :text-is("user: ${prompt}")`, read: "text", pattern: `^user: (${prompt})$`, group: 1 },
    expected: prompt,
    normalize: "whitespace",
  };
}
export const modelSource: ModelBrowserSource = {
  revision: 0, defaultThinkingLevel: "off", modelProviderOrder: [], knownRoleIds: ["default", "smol"],
  mruOrder: [], modelPerf: new Map(),
  getRoleInfo: role => ({ tag: role, name: `${role} role`, section: "chat", accepts: () => true }),
  defaultRoleChain: () => [], getModelRole: () => undefined,
  resolveRoleValue: (value, models) => ({ model: models.find(model => `${model.provider}/${model.id}` === value), explicitThinkingLevel: false }),
};

const queryProperty = "property:TspPickerProps.query";
export function pickerQueryProof(query: string, selector: string): Comparison {
  return { name: "actual picker query", terminal: { type: "screen", pattern: `(${query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`, group: 1 }, browser: { type: "dom", selector, read: "value" }, expected: query, proves: [{ entry: queryProperty, effect: "literal", value: query }] };
}

export const fixtureCases: FixtureCase[] = [
  ...(["model", "role", "cancel"] as const).map((mode): FixtureCase => ({
    id: `picker-model-${mode}`,
    covers: [component("model-picker", "ModelPickerComponent"), component("model-browser", "ModelBrowser"), ...(mode === "cancel" ? [] : [queryProperty])],
    behaviors: ["picker:selection", `picker:${mode}`],
    async create(context) {
      const { buildModel } = await context.load<typeof CatalogBuild>("packages/catalog/src/build.ts");
      const { ModelPickerComponent } = await context.load<typeof ModelPicker>(`${source}model-picker.ts`);
      const models = ["Alpha coder", "Beta coder"].map((name, index) => buildModel({
        id: index === 0 ? "alpha" : "beta", name, provider: "parity", api: "openai-completions",
        baseUrl: "http://127.0.0.1:1/v1", reasoning: false, input: ["text"],
        cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 32000, maxTokens: 4096,
      }));
      return new ModelPickerComponent(context.tui, modelSource, {
        getError: () => undefined, getAvailable: () => models, getAll: () => models, refreshIfStale: async () => false,
      }, models.map(model => ({ model })), {
        onPick: (_model, selector, meta) => { context.record("pick", { selector, overContext: meta.overContext }); context.done(); },
        onPickRole: entry => { context.record("role", entry.role); context.done(); },
        onCancel: () => { context.record("cancel", true); context.done(); },
      }, { currentSelector: "parity/alpha", quickRoles: [{ role: "smol", model: models[1], explicitThinkingLevel: false }], quickRoleOrder: ["smol"] });
    },
    checkpoints: [
      { name: "catalog", comparisons: [visibleText("model", "(Alpha coder)", 'text="Alpha coder"', "Alpha coder")] },
      ...(mode === "cancel" ? [{ name: "cancel", terminalInput: "\x1b", browserActions: [{ type: "press" as const, key: "Escape" }], comparisons: [callbackValue("cancelled", "cancel", true)] }] : mode === "role" ? [
        { name: "quick role search", terminalInput: "@smol", browserActions: [{ type: "fill" as const, selector: 'input[placeholder="Search models, @ for quick roles…"]', value: "@smol" }], comparisons: [visibleText("role", "(@smol)", '[data-omp-picker-item="@smol"] :text-is("@smol")', "@smol"), pickerQueryProof("@smol", 'input[placeholder="Search models, @ for quick roles…"]')] },
        { name: "choose role", terminalInput: "\r", browserActions: [{ type: "press" as const, key: "Enter", selector: 'input[placeholder="Search models, @ for quick roles…"]' }], comparisons: [callbackValue("chosen role", "role", "smol")] },
      ] : [
        { name: "filter", terminalInput: "Beta", browserActions: [{ type: "fill" as const, selector: 'input[placeholder="Search models, @ for quick roles…"]', value: "Beta" }], comparisons: [visibleText("matching model", "(Beta coder)", 'text="Beta coder"', "Beta coder"), pickerQueryProof("Beta", 'input[placeholder="Search models, @ for quick roles…"]')] },
        { name: "choose model", terminalInput: "\r", browserActions: [{ type: "press" as const, key: "Enter", selector: 'input[placeholder="Search models, @ for quick roles…"]' }], comparisons: [callbackValue("chosen model", "pick", { selector: "parity/beta", overContext: false })] },
      ]),
    ],
  })),
  ...(["select", "cancel"] as const).map((mode): FixtureCase => ({
    id: `picker-history-${mode}`,
    covers: [component("history-search", "HistorySearchComponent"), component("history-search", "HistoryResultsList"), ...(mode === "select" ? [queryProperty] : [])],
    behaviors: ["picker:history", `picker:${mode}`, ...(mode === "select" ? ["picker:query-host-refinement"] : [])],
    async create(context) {
      const { HistorySearchComponent } = await context.load<typeof HistorySearch>(`${source}history-search.ts`);
      const entries = [{ prompt: "deploy the release\nwith notes", created_at: 1794000000, cwd: context.workspace }, { prompt: "routine status", created_at: 1793990000 }];
      const selector = new HistorySearchComponent({ getRecent: limit => entries.slice(0, limit), search: (query, limit) => { context.record("history-query", query); return entries.filter(entry => entry.prompt.includes(query)).slice(0, limit); } }, prompt => { context.record("prompt", prompt); context.done(); }, () => { context.record("cancel", true); context.done(); });
      selector.setOnRequestRender(() => context.tui.requestRender());
      return selector;
    },
    checkpoints: [
      { name: "history contents", comparisons: [visibleText("prompt", "(routine status)", 'text="routine status"', "routine status")] },
      ...(mode === "select" ? [
        { name: "search", terminalInput: "deploy", browserActions: [{ type: "fill" as const, selector: 'input[placeholder="Search prompts…"]', value: "deploy" }], comparisons: [visibleText("matching prompt", "(deploy the release with notes)", 'text="deploy the release with notes"', "deploy the release with notes"), pickerQueryProof("deploy", 'input[placeholder="Search prompts…"]'), callbackValue("host search request", "history-query", "deploy")] },
        { name: "refine real history query", terminalInput: "\x15routine", browserActions: [{ type: "fill" as const, selector: 'input[placeholder="Search prompts…"]', value: "routine" }], comparisons: [
          { ...pickerQueryProof("routine", 'input[placeholder="Search prompts…"]'), proves: [{ entry: queryProperty, effect: "host" as const, value: "routine", before: { checkpoint: "search", comparison: "actual picker query" }, assertionGroups: [{ comparisons: [{ checkpoint: "refine real history query", comparison: "actual picker query" }, { checkpoint: "refine real history query", comparison: "host search request" }] }] }] },
          callbackValue("host search request", "history-query", "routine"), visibleText("refined history result", "(routine status)", 'text="routine status"', "routine status"),
        ] },
        { name: "restore multiline history query", terminalInput: "\x15deploy", browserActions: [{ type: "fill" as const, selector: 'input[placeholder="Search prompts…"]', value: "deploy" }], comparisons: [pickerQueryProof("deploy", 'input[placeholder="Search prompts…"]'), callbackValue("restored host search request", "history-query", "deploy"), visibleText("multiline prompt result", "(deploy the release with notes)", 'text="deploy the release with notes"', "deploy the release with notes")] },
      ] : []),
      { name: mode, terminalInput: mode === "select" ? "\r" : "\x1b", browserActions: [{ type: "click", selector: `button:has-text("${mode === "select" ? "Insert" : "Close"}")` }], comparisons: [callbackValue("result", mode === "select" ? "prompt" : "cancel", mode === "select" ? "deploy the release\nwith notes" : true)] },
    ],
  })),
  ...(["select", "cancel", "delete", "delete-cancel", "delete-error"] as const).map((mode): FixtureCase => ({
    id: `picker-session-${mode}`,
    covers: [component("session-selector", "SessionSelectorComponent"), component("session-selector", "SessionList"), ...(mode.startsWith("delete") ? [component("hook-selector", "HookSelectorComponent"), "property:TspPickerProps.confirm.label"] : []), ...(mode === "delete-error" ? ["property:TspPickerProps.message"] : [])],
    behaviors: ["picker:session", `picker:${mode}`, ...(mode.startsWith("delete") ? ["picker:delete-confirmation-ownership"] : [])],
    async create(context) {
      const { SessionSelectorComponent } = await context.load<typeof SessionSelector>(`${source}session-selector.ts`);
      const sessions = ["Alpha", "Beta"].map((title, index) => ({ path: `${context.workspace}/${title}.jsonl`, id: title.toLowerCase(), cwd: context.workspace, title, modified: new Date(1794000000000 - index * 1000), size: 5600, firstMessage: `${title} prompt`, allMessagesText: `${title} prompt ${title} answer`, status: "complete" as const }));
      await Promise.all(sessions.map(session => writeFile(session.path, JSON.stringify({ type: "session", version: 3, id: session.id, timestamp: "2026-11-01T12:00:00Z", cwd: "." }) + "\n")));
      const selector = new SessionSelectorComponent(sessions, session => { context.record("resume", session.id); context.done(); }, () => { context.record("cancel", true); context.done(); }, () => context.done(), {
        title: "Fixture sessions", scopeLabel: false, getTerminalRows: () => 40,
        onDelete: async session => {
          context.record("delete", session.id);
          if (mode === "delete-error") throw new Error("Fixture deletion refused");
          await unlink(session.path);
          context.record("deleted-file", !existsSync(session.path));
          return true;
        },
      });
      selector.setOnRequestRender(() => context.tui.requestRender());
      return selector;
    },
    checkpoints: [
      { name: "session information", comparisons: [visibleText("session", "(Alpha)", '[data-omp-picker-item] :text-is("Alpha")', "Alpha")] },
      ...(mode.startsWith("delete") ? [
        { name: "open confirmation", terminalInput: "\x7f", browserActions: [{ type: "click" as const, selector: 'button:has-text("Delete")' }], comparisons: [{ ...visibleText("confirmation operation", "(Delete) session\\?", '[data-omp-picker-confirm] button', "Delete"), proves: [{ entry: "property:TspPickerProps.confirm.label", effect: "literal" as const, value: "Delete" }] }, { name: "confirmation target", terminal: { type: "screen" as const, pattern: "Delete session\\?[\\s\\S]*?(Alpha)", group: 1 }, browser: { type: "dom" as const, selector: "[data-omp-picker-confirm]", read: "text" as const, pattern: "Delete “(Alpha)”", group: 1 }, expected: "Alpha" }] },
        { name: "confirm or dismiss", terminalInput: mode === "delete-cancel" ? "\x1b" : "\r", browserActions: mode === "delete-cancel" ? [{ type: "press" as const, key: "Escape" }] : [{ type: "click" as const, selector: '[data-omp-picker-confirm] button' }], comparisons: mode === "delete-cancel" ? [
          visibleText("retained", "(Alpha)", '[data-omp-picker-item] :text-is("Alpha")', "Alpha"),
          { name: "retained session file", terminal: { type: "file" as const, path: "Alpha.jsonl", pointer: "/id" }, browser: { type: "file" as const, path: "Alpha.jsonl", pointer: "/id" }, expected: "alpha" },
        ] : mode === "delete-error" ? [
          { ...visibleText("error", "(Error: Fixture deletion refused)", 'text="Error: Fixture deletion refused"', "Error: Fixture deletion refused"), proves: [{ entry: "property:TspPickerProps.message", effect: "literal" as const, value: "Error: Fixture deletion refused" }] }, callbackValue("attempted delete", "delete", "alpha"),
          { name: "failed deletion retains file", terminal: { type: "file" as const, path: "Alpha.jsonl", pointer: "/id" }, browser: { type: "file" as const, path: "Alpha.jsonl", pointer: "/id" }, expected: "alpha" },
        ] : [visibleText("remaining session", "(Beta)", '[data-omp-picker-item] :text-is("Beta")', "Beta"), callbackValue("deleted", "delete", "alpha"), callbackValue("actual file deletion", "deleted-file", true)] },
      ] : [{ name: mode, terminalInput: mode === "select" ? "\r" : "\x1b", browserActions: [{ type: "click" as const, selector: `button:has-text("${mode === "select" ? "Resume" : "Close"}")` }], comparisons: [callbackValue("result", mode === "select" ? "resume" : "cancel", mode === "select" ? "alpha" : true)] }]),
    ],
  })),
  ...(["select", "cancel", "label", "label-cancel"] as const).map((mode): FixtureCase => ({
    id: `picker-tree-${mode}`,
    covers: [component("tree-selector", "TreeSelectorComponent"), component("tree-selector", "TreeList"), ...(mode.startsWith("label") ? [component("tree-selector", "LabelInput")] : [])],
    async create(context) {
      const { TreeSelectorComponent } = await context.load<typeof TreeSelector>(`${source}tree-selector.ts`);
      const tree: TreeSelector.TreeSelectorNode[] = [{ entry: { type: "message", id: "u1", parentId: null, timestamp: "2026-11-01T12:00:00Z", message: { role: "user", content: "Find the frame", timestamp: 1 } }, children: [{ entry: { type: "message", id: "u2", parentId: "u1", timestamp: "2026-11-01T12:01:00Z", message: { role: "user", content: "Fix the acknowledgement", timestamp: 2 } }, children: [] }] }];
      const labels: { id: string; label: string | undefined }[] = [];
      return new TreeSelectorComponent(tree, "u2", 40, (id, options) => { context.record("select", { id, summarize: options.summarize }); context.done(); }, () => { context.record("labels", labels); context.record("cancel", true); context.done(); }, (id, label) => {
        labels.push({ id, label });
        writeFileSync(`${context.workspace}/tree-labels.json`, JSON.stringify(labels));
        context.record("label", { id, label });
      });
    },
    checkpoints: [
      { name: "tree contents", comparisons: [treeUserPrompt("user prompt", "u1", "Find the frame")] },
      ...(mode.startsWith("label") ? [
        { name: "label editor", terminalInput: "L", browserActions: [{ type: "click" as const, selector: 'button:text-is("Label")' }], comparisons: [visibleText("label prompt", "(Label) \\(empty to remove\\)", '[data-omp-role="omp.picker.title"]:text-is("Label")', "Label")] },
        { name: "label draft", terminalInput: "Fixture label", browserActions: [{ type: "fill" as const, selector: 'input:not([placeholder="Search entries…"])', value: "Fixture label" }], comparisons: [{ name: "draft", terminal: { type: "screen" as const, pattern: "(Fixture label)", group: 1 }, browser: { type: "dom" as const, selector: 'input:not([placeholder="Search entries…"])', read: "value" as const }, expected: "Fixture label" }] },
        { name: mode, terminalInput: mode === "label" ? "\r" : "\x1b", browserActions: [{ type: "click" as const, selector: `button:text-is("${mode === "label" ? "Save label" : "Cancel"}")` }], comparisons: mode === "label" ? [callbackValue("label mutation", "label", { id: "u2", label: "Fixture label" }), { name: "persisted label", terminal: { type: "file" as const, path: "tree-labels.json", pointer: "/0/label" }, browser: { type: "file" as const, path: "tree-labels.json", pointer: "/0/label" }, expected: "Fixture label" }] : [treeUserPrompt("unchanged prompt", "u2", "Fix the acknowledgement")] },
      ] : []),
      ...(mode === "select" ? [
        { name: "move to previous turn", terminalInput: "\x1b[A", browserActions: [{ type: "click" as const, selector: '[data-omp-picker-item="u1"]' }], comparisons: [{
          name: "highlighted turn", terminal: { type: "screen" as const, pattern: "› . user: (Find the frame)", group: 1 },
          browser: { type: "dom" as const, selector: '[data-omp-picker-item="u1"][aria-selected="true"] :text-is("user: Find the frame")', read: "text" as const, pattern: "^user: (Find the frame)$", group: 1 },
          expected: "Find the frame", normalize: "whitespace" as const,
        }] },
        { name: "select previous turn", terminalInput: "\r", browserActions: [{ type: "press" as const, key: "Enter", selector: '[data-omp-picker-item="u1"]' }], comparisons: [callbackValue("selected entry", "select", { id: "u1", summarize: false })] },
      ] : []),
      ...(mode === "select" ? [] : [
        { name: "close owner", terminalInput: "\x1b", browserActions: [{ type: "press" as const, key: "Escape" }], comparisons: [callbackValue("owner closed", "cancel", true), callbackValue("retained labels", "labels", mode === "label" ? [{ id: "u2", label: "Fixture label" }] : [])] },
      ]),
    ],
  })),
  {
    id: "picker-queue-mode-commit",
    covers: [component("queue-mode-selector", "QueueModeSelectorComponent")],
    async create(context) {
      const { QueueModeSelectorComponent } = await context.load<typeof QueueModeSelector>(`${source}queue-mode-selector.ts`);
      const selector = new QueueModeSelectorComponent("one-at-a-time", mode => { context.record("mode", mode); context.done(); }, () => context.record("cancel", true));
      return Object.assign(selector, { handleInput: (data: string) => selector.getSelectList().handleInput(data) });
    },
    checkpoints: [
      { name: "current mode", comparisons: [{ name: "current value", terminal: { type: "screen", pattern: "(one-at-a-time)", group: 1 }, browser: { type: "dom", selector: '[data-omp-picker-item="one-at-a-time"]', read: "attribute", attribute: "data-omp-picker-item" }, expected: "one-at-a-time" }] },
      { name: "move to all", terminalInput: "\x1b[B", browserActions: [{ type: "click", selector: '[data-omp-picker-item="all"]' }], comparisons: [{ name: "highlighted mode", terminal: { type: "screen", pattern: "[>›→❯▶] (all)", group: 1 }, browser: { type: "dom", selector: '[data-omp-picker-item="all"][aria-selected="true"]', read: "attribute", attribute: "data-omp-picker-item" }, expected: "all" }] },
      { name: "choose all", terminalInput: "\r", browserActions: [{ type: "press", key: "Enter", selector: '[data-omp-picker-item="all"]' }], comparisons: [callbackValue("mode", "mode", "all")] },
    ],
  },
  ...agentCases,
  ...modelCases,
  ...hubCases,
  ...treeCases,
  ...accountCases,
  ...sessionCases,
];
