import type * as Splash from "@omp-source/packages/tui/src/setup/startup-splash";
import type * as Cleanse from "@omp-source/packages/tui/src/overlays/cleanse-panel";
import type * as Board from "@omp-source/packages/tui/src/apps/cleanse-board";
import type * as GitApp from "@omp-source/packages/tui/src/apps/git/git-tui";
import type { ChangedFile, GitTuiModel } from "@omp-source/packages/tui/src/apps/git/state";
import type * as Dashboard from "@omp-source/packages/tui/src/apps/autoresearch-dashboard";
import type * as Research from "@omp-source/packages/tui/src/tools/autoresearch";
import type * as Live from "@omp-source/packages/tui/src/apps/live-visualizer";
import type * as Probe from "@omp-source/packages/tui/src/apps/debug/protocol-probe";
import type * as Capabilities from "@omp-source/packages/tui/src/terminal-capabilities";
import type * as AgentsHub from "@omp-source/packages/tui/src/overlays/agents-hub";
import type { ModelBrowserSource } from "@omp-source/packages/tui/src/overlays/model-browser";
import type * as ShowImages from "@omp-source/packages/tui/src/overlays/show-images-selector";
import type * as ThemeSelector from "@omp-source/packages/tui/src/overlays/theme-selector";
import type * as ThinkingSelector from "@omp-source/packages/tui/src/overlays/thinking-selector";
import type * as TreeSelector from "@omp-source/packages/tui/src/overlays/tree-selector";
import type { PluginSettingsHost, InstalledPlugin, InstalledPluginSummary } from "@omp-source/packages/tui/src/overlays/plugin-settings";
import { createSettings } from "./settings-support";
import { panelControls } from "./panel-dialogs";
import { callbackValue, visibleText, type FixtureCase } from "../scenario";

const browserSource: ModelBrowserSource = {
  revision: 0, defaultThinkingLevel: "off", modelProviderOrder: [], knownRoleIds: [], mruOrder: [], modelPerf: new Map(),
  getRoleInfo: role => ({ name: role, section: "chat", accepts: () => true }), getModelRole: () => undefined,
  defaultRoleChain: () => [], resolveRoleValue: () => ({ model: undefined, explicitThinkingLevel: false }),
};
const agentsHubFixture = (flow: string): FixtureCase["create"] => async context => {
  const { AgentsHubComponent } = await context.load<typeof AgentsHub>("packages/tui/src/overlays/agents-hub.ts");
  const agent: AgentsHub.HubAgent = { name: "FixtureScout", description: "Read only fixture research", systemPrompt: "Inspect fixture data", source: "project", disabled: false };
  return AgentsHubComponent.create(context.tui, {
    browserSource, loadAgents: async () => [agent], getAvailableModels: () => [], effectiveModelPatterns: () => [], resolvePatterns: () => undefined,
    effectivePrewalkPattern: () => undefined, effectiveAdvisorPattern: () => undefined,
    setAgentDisabled: () => context.record(flow, "disabled"),
    setAgentOverride: (property, name, value) => context.record("override", { property, name, value }),
    generateAgent: async () => { throw new Error("Fixture forbids model generation"); },
    saveAgent: async () => { throw new Error("Fixture forbids writes outside its constructor state"); },
  }, { onCancel: () => { context.record("cancel", true); context.done(); } });
};

const pickerTitle = (title: string) => ({
  name: "picker title", terminal: { type: "screen" as const, pattern: `(${title})`, group: 1 },
  browser: { type: "dom" as const, selector: '[role="dialog"] h3', read: "text" as const, pattern: `^(${title})$`, group: 1 }, expected: title,
});
const pickedItem = (value: string, label = value) => ({
  name: "highlighted item", terminal: { type: "screen" as const, pattern: `[>›→❯▶] (${label})`, group: 1 },
  browser: { type: "dom" as const, selector: `[data-omp-picker-item="${value}"][aria-selected="true"]`, read: "attribute" as const, attribute: "data-omp-picker-item" }, expected: value,
});
const researchResult = (runNumber: number, metric: number, description: string): Research.ExperimentResult => ({
  runNumber, commit: `c0ffee${runNumber}`, metric, metrics: {}, status: "keep", description, timestamp: runNumber, segment: 0, confidence: null,
  modifiedPaths: [], scopeDeviations: [], justification: null, flagged: false, flaggedReason: null,
});
const researchRuntime = (): Dashboard.AutoresearchDashboardRuntime => {
  const results = [researchResult(1, 24, "Baseline parser"), researchResult(2, 20, "Faster tokenizer loop")];
  const state: Research.ExperimentState = {
    results, bestMetric: 20, bestDirection: "lower", metricName: "p50_ms", metricUnit: "ms", secondaryMetrics: [], name: "Speed up the parser",
    goal: "Cut parse time.", currentSegment: 0, maxExperiments: null, confidence: null, scopePaths: [], offLimits: [], constraints: [], notes: "",
    branch: null, baselineCommit: null, sessionId: null,
  };
  return { autoresearchMode: true, dashboardExpanded: true, state, lastRunSummary: null, runningExperiment: null };
};

const component = (file: string, name: string) => `component:packages/tui/src/${file}.ts#${name}`;

export const fixtureCases: FixtureCase[] = [
  {
    id: "settings-plugins-tab-detail-config",
    covers: [
      component("overlays/plugin-settings", "PluginSettingsComponent"), component("overlays/plugin-settings", "PluginListComponent"),
      component("overlays/plugin-settings", "PluginDetailComponent"), component("overlays/plugin-settings", "ConfigFieldPanel"),
      component("overlays/plugin-settings", "MarketplacePluginDetailComponent"),
    ],
    async create(context) {
      const plugin: InstalledPlugin = {
        name: "fixture-plugin", version: "1.0.0", enabled: true, enabledFeatures: ["fast"],
        manifest: { description: "Fixture plugin", features: { fast: { description: "Fast path", default: true } }, settings: { greeting: { type: "string", description: "Greeting text", default: "hello" } } },
      };
      const summary: InstalledPluginSummary = { id: "market-fixture@fixture-market", scope: "user", entries: [{ installPath: `${context.workspace}/market`, version: "2.0.0", installedAt: "2026-11-01T12:00:00Z", lastUpdated: "2026-11-01T12:00:00Z", enabled: true }] };
      const values: Record<string, unknown> = {};
      const plugins: PluginSettingsHost = {
        manager: {
          list: async () => [plugin], getPlugin: async () => plugin, getPluginSettings: async () => values,
          setEnabled: async (name, enabled) => { plugin.enabled = enabled; context.record("enabled", { name, enabled }); },
          getEnabledFeatures: async () => plugin.enabledFeatures, setEnabledFeatures: async (_name, features) => { plugin.enabledFeatures = features; },
          setPluginSetting: async (name, key, value) => { values[key] = value; context.record("plugin-setting", { name, key, value }); },
        },
        createMarketplaceManager: async () => ({ listInstalledPlugins: async () => [summary], setPluginEnabled: async () => undefined }),
        parsePluginId: id => ({ name: id.split("@")[0] ?? id }),
      };
      return createSettings(context, undefined, {}, { plugins });
    },
    checkpoints: [
      { name: "open plugins page", terminalInput: "\t".repeat(10), browserActions: [{ type: "click", selector: 'button:text-is("Installed")' }], comparisons: [
        { name: "npm plugin row", terminal: { type: "screen", pattern: "(fixture-plugin)", group: 1 }, browser: { type: "dom", selector: '[data-omp-setting="npm:fixture-plugin"]', read: "text", pattern: "(fixture-plugin)", group: 1 }, expected: "fixture-plugin" },
        { name: "marketplace plugin row", terminal: { type: "screen", pattern: "(market-fixture@fixture-market)", group: 1 }, browser: { type: "dom", selector: '[data-omp-setting="mkt:user:market-fixture@fixture-market"]', read: "text", pattern: "(market-fixture@fixture-market)", group: 1 }, expected: "market-fixture@fixture-market" },
      ] },
    ],
  },
  {
    id: "picker-tree-search-filter",
    covers: [component("overlays/tree-selector", "SearchLine"), component("overlays/tree-selector", "TreeSelectorComponent")],
    async create(context) {
      const { TreeSelectorComponent } = await context.load<typeof TreeSelector>("packages/tui/src/overlays/tree-selector.ts");
      const user = (id: string, parentId: string | null, text: string, minute: number) => ({ entry: { type: "message" as const, id, parentId, timestamp: `2026-11-01T12:0${minute}:00Z`, message: { role: "user" as const, content: text, timestamp: minute } }, children: [] as TreeSelector.TreeSelectorNode[] });
      const second = user("u2", "u1", "Fix the acknowledgement", 1);
      const first = user("u1", null, "Find the frame", 0);
      first.children.push(second);
      return new TreeSelectorComponent([first], "u2", 40, () => context.done(), () => { context.record("cancel", true); context.done(); }, () => undefined);
    },
    checkpoints: [
      { name: "both turns", comparisons: [{ name: "first turn", terminal: { type: "screen", pattern: "user: (Find the frame)", group: 1 }, browser: { type: "dom", selector: '[data-omp-picker-item="u1"] :text-is("user: Find the frame")', read: "text", pattern: "^user: (Find the frame)$", group: 1 }, expected: "Find the frame" }] },
      { name: "filter", terminalInput: "frame", browserActions: [{ type: "fill", selector: 'input[placeholder="Search entries…"]', value: "frame" }], comparisons: [
        { name: "search draft", terminal: { type: "screen", pattern: "Search: (frame)", group: 1 }, browser: { type: "dom", selector: 'input[placeholder="Search entries…"]', read: "value" }, expected: "frame" },
        { name: "filtered selection", terminal: { type: "screen", pattern: "› . user: (Find the frame)", group: 1 }, browser: { type: "dom", selector: '[data-omp-picker-item="u1"][aria-selected="true"] :text-is("user: Find the frame")', read: "text", pattern: "^user: (Find the frame)$", group: 1 }, expected: "Find the frame" },
      ] },
      { name: "cancel", terminalInput: "\x1b\x1b", browserActions: [{ type: "click", selector: 'button:has-text("Close") >> nth=0' }], comparisons: [callbackValue("cancelled", "cancel", true)] },
    ],
  },
  {
    id: "picker-show-images-select",
    covers: [component("overlays/show-images-selector", "ShowImagesSelectorComponent")],
    async create(context) {
      const { ShowImagesSelectorComponent } = await context.load<typeof ShowImages>("packages/tui/src/overlays/show-images-selector.ts");
      const selector = new ShowImagesSelectorComponent(true, show => { context.record("show", show); context.done(); }, () => context.record("cancel", true));
      return Object.assign(selector, { handleInput: (data: string) => selector.getSelectList().handleInput(data) });
    },
    checkpoints: [
      { name: "options", comparisons: [pickerTitle("Show Images"), pickedItem("yes", "Yes")] },
      { name: "move to no", terminalInput: "\x1b[B", browserActions: [{ type: "click", selector: '[data-omp-picker-item="no"]' }], comparisons: [pickedItem("no", "No")] },
      { name: "choose no", terminalInput: "\r", browserActions: [{ type: "press", key: "Enter", selector: '[data-omp-picker-item="no"]' }], comparisons: [callbackValue("show images", "show", false)] },
    ],
  },
  {
    id: "picker-theme-select",
    covers: [component("overlays/theme-selector", "ThemeSelectorComponent")],
    async create(context) {
      const { ThemeSelectorComponent } = await context.load<typeof ThemeSelector>("packages/tui/src/overlays/theme-selector.ts");
      const selector = new ThemeSelectorComponent("dark", ["dark", "light", "mono"], name => { context.record("theme", name); context.done(); }, () => context.record("cancel", true), name => context.record("preview", name));
      return Object.assign(selector, { handleInput: (data: string) => selector.getSelectList().handleInput(data) });
    },
    checkpoints: [
      { name: "themes", comparisons: [pickerTitle("Theme"), pickedItem("dark")] },
      { name: "preview light", terminalInput: "\x1b[B", browserActions: [{ type: "click", selector: '[data-omp-picker-item="light"]' }], comparisons: [pickedItem("light"), callbackValue("previewed theme", "preview", "light")] },
      { name: "apply light", terminalInput: "\r", browserActions: [{ type: "press", key: "Enter", selector: '[data-omp-picker-item="light"]' }], comparisons: [callbackValue("applied theme", "theme", "light")] },
    ],
  },
  {
    id: "picker-thinking-select",
    covers: [component("overlays/thinking-selector", "ThinkingSelectorComponent")],
    async create(context) {
      const { ThinkingSelectorComponent } = await context.load<typeof ThinkingSelector>("packages/tui/src/overlays/thinking-selector.ts");
      const selector = new ThinkingSelectorComponent("off", ["off", "low", "high"] as never, level => { context.record("level", level); context.done(); }, () => context.record("cancel", true));
      return Object.assign(selector, { handleInput: (data: string) => selector.getSelectList().handleInput(data) });
    },
    checkpoints: [
      { name: "levels", comparisons: [pickerTitle("Thinking Level"), pickedItem("off")] },
      { name: "move to high", terminalInput: "\x1b[B\x1b[B", browserActions: [{ type: "click", selector: '[data-omp-picker-item="high"]' }], comparisons: [pickedItem("high")] },
      { name: "choose high", terminalInput: "\r", browserActions: [{ type: "press", key: "Enter", selector: '[data-omp-picker-item="high"]' }], comparisons: [callbackValue("thinking level", "level", "high")] },
    ],
  },
  {
    id: "app-agents-hub-create-back-cancel",
    covers: [component("overlays/agents-hub", "AgentsHubComponent"), component("overlays/hub-frame", "HubFrame")],
    create: agentsHubFixture("create"),
    checkpoints: [
      { name: "agent list", comparisons: [visibleText("agent", "(FixtureScout)", '[data-omp-picker-item="agent:project:FixtureScout"] :text-is("FixtureScout")', "FixtureScout")] },
      { name: "open create wizard", terminalInput: "\x1b[B\r", browserActions: [{ type: "click", selector: '[data-omp-picker-item="new"]' }, { type: "click", selector: 'button:not([data-omp-picker-item]):has-text("New agent")' }], comparisons: [visibleText("wizard title", "(Create new agent)", 'text="Create new agent"', "Create new agent"), { name: "wizard scope", terminal: { type: "screen", pattern: "Describe what the agent should do; scope: (project)", group: 1 }, browser: { type: "dom", selector: 'p:has-text("Describe what the agent should do; scope:")', read: "text", pattern: "scope: (project)", group: 1 }, expected: "project" }] },
      { name: "back to list", terminalInput: "\x1b", browserActions: [{ type: "press", key: "Escape" }], comparisons: [visibleText("agent", "(FixtureScout)", '[data-omp-picker-item="agent:project:FixtureScout"] :text-is("FixtureScout")', "FixtureScout")] },
      { name: "cancel", terminalInput: "\x1b", browserActions: [{ type: "press", key: "Escape" }], comparisons: [callbackValue("cancelled", "cancel", true)] },
    ],
  },
  {
    id: "app-agents-hub-pattern-strip-cancel",
    covers: [component("overlays/agents-hub", "AgentsHubComponent"), component("overlays/hub-frame", "HubFrame")],
    create: agentsHubFixture("pattern"),
    checkpoints: [
      { name: "agent list", comparisons: [visibleText("agent", "(FixtureScout)", '[data-omp-picker-item="agent:project:FixtureScout"] :text-is("FixtureScout")', "FixtureScout")] },
      { name: "open property chips", terminalInput: "\r", browserActions: [{ type: "click", selector: '[data-omp-picker-item="agent:project:FixtureScout"]' }, { type: "press", key: "Enter" }], comparisons: [visibleText("strip chip", "\\[ (model: auto) \\]", 'button:text-is("model: auto")', "model: auto")] },
      { name: "open model choices", terminalInput: "\r", browserActions: [{ type: "press", key: "Enter" }], comparisons: [visibleText("model strip", "(FixtureScout · model →)", 'text="FixtureScout · model →"', "FixtureScout · model →")] },
      { name: "open pattern input", terminalInput: "\x1b[C\r", browserActions: [{ type: "press", key: "ArrowRight" }, { type: "press", key: "Enter" }], comparisons: [visibleText("pattern strip", "(FixtureScout model pattern:)", 'text="FixtureScout model pattern:"', "FixtureScout model pattern:")] },
      { name: "close pattern", terminalInput: "\x1b", browserActions: [{ type: "press", key: "Escape" }], comparisons: [visibleText("agent", "(FixtureScout)", '[data-omp-picker-item="agent:project:FixtureScout"] :text-is("FixtureScout")', "FixtureScout")] },
    ],
  },
  {
    id: "app-autoresearch-overlay-close",
    covers: [component("apps/autoresearch-dashboard", "createDashboardController.showOverlay")],
    async create(context) {
      const { createDashboardController } = await context.load<typeof Dashboard>("packages/tui/src/apps/autoresearch-dashboard.ts");
      let sheet: Awaited<ReturnType<FixtureCase["create"]>> | undefined;
      void createDashboardController().showOverlay({
        hasUI: true,
        ui: {
          setWidget: () => undefined,
          custom: (factory) => new Promise(resolve => {
            sheet = factory({ requestRender: () => context.tui.requestRender() }, context.theme, undefined, () => { context.record("close", true); resolve(undefined as never); context.done(); }) as typeof sheet;
          }),
        },
      }, researchRuntime());
      if (!sheet) throw new Error("The dashboard opened no overlay");
      return sheet;
    },
    checkpoints: [
      { name: "experiment runs", comparisons: [{ name: "latest run", terminal: { type: "screen", pattern: "(Faster tokenizer loop)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.autoresearch.runs"]', read: "text", pattern: "(Faster tokenizer loop)", group: 1 }, expected: "Faster tokenizer loop" }] },
      { name: "close", terminalInput: "q", browserActions: [{ type: "click", selector: 'button[title^="Close"] >> nth=-1' }], comparisons: [callbackValue("closed", "close", true)] },
    ],
  },
  {
    id: "app-autoresearch-widget",
    covers: [component("apps/autoresearch-dashboard", "DashboardWidget")],
    async create(context) {
      const { createDashboardController } = await context.load<typeof Dashboard>("packages/tui/src/apps/autoresearch-dashboard.ts");
      let widget: Awaited<ReturnType<FixtureCase["create"]>> | undefined;
      createDashboardController().updateWidget({
        hasUI: true,
        ui: {
          setWidget: (_key, factory) => { widget = factory?.({ requestRender: () => context.tui.requestRender() }, context.theme) as typeof widget; },
          custom: () => Promise.reject(new Error("The widget never opens an overlay")),
        },
      }, researchRuntime());
      if (!widget) throw new Error("The dashboard registered no widget");
      return widget;
    },
    checkpoints: [
      { name: "widget title", comparisons: [{ name: "experiment name", terminal: { type: "screen", pattern: "(autoresearch: Speed up the parser)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.widget.autoresearch"]', read: "text", pattern: "(autoresearch: Speed up the parser)", group: 1 }, expected: "autoresearch: Speed up the parser" }] },
    ],
  },
  {
    id: "app-live-visualizer-mute-end",
    covers: [component("apps/live-visualizer", "LiveVisualizer")],
    async create(context) {
      const { LiveVisualizer } = await context.load<typeof Live>("packages/tui/src/apps/live-visualizer.ts");
      const visualizer = new LiveVisualizer({ onStop: () => { context.record("stop", true); context.done(); }, onToggleMute: () => context.record("mute", true) });
      visualizer.setPhase("listening");
      visualizer.setInputLevel(0.4);
      visualizer.setFrame(3);
      visualizer.setTranscript("hello fixture");
      return visualizer;
    },
    checkpoints: [
      { name: "call state", comparisons: [
        { name: "phase", terminal: { type: "screen", pattern: "(listening)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.app.live"]', read: "text", pattern: "(listening)", group: 1 }, expected: "listening" },
        { name: "transcript", terminal: { type: "screen", pattern: "(hello fixture)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.app.live"]', read: "text", pattern: "(hello fixture)", group: 1 }, expected: "hello fixture" },
      ] },
      { name: "mute", terminalInput: " ", browserActions: [{ type: "click", selector: 'button:has-text("Mute")' }], comparisons: [callbackValue("mute toggled", "mute", true)] },
      { name: "end call", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button:has-text("End call")' }], comparisons: [callbackValue("stopped", "stop", true)] },
    ],
  },
  {
    id: "debug-protocol-probe-text-sizing",
    covers: [component("apps/debug/protocol-probe", "ProtocolProbeComponent"), component("apps/debug/protocol-probe", "RawLines")],
    async create(context) {
      const capabilities = await context.load<typeof Capabilities>("packages/tui/src/terminal-capabilities.ts");
      const { ProtocolProbeComponent, buildSampleImage } = await context.load<typeof Probe>("packages/tui/src/apps/debug/protocol-probe.ts");
      capabilities.setTerminalTextSizing(true);
      return new ProtocolProbeComponent({ image: buildSampleImage(), imageBudget: context.tui.imageBudget, notificationSuppressed: true });
    },
    checkpoints: [
      { name: "probe report", comparisons: [
        visibleText("title", "(Terminal Protocol Test)", 'text="Terminal Protocol Test"', "Terminal Protocol Test"),
        { name: "notification", terminal: { type: "screen", pattern: "(suppressed \\(PI_NOTIFICATIONS\\))", group: 1 }, browser: { type: "dom", selector: "body", read: "text", pattern: "(suppressed \\(PI_NOTIFICATIONS\\))", group: 1 }, expected: "suppressed (PI_NOTIFICATIONS)" },
      ] },
    ],
  },
  {
    id: "app-git-diff-help-commit",
    covers: [component("apps/git/git-tui", "GitTuiComponent"), component("apps/git/sidebar", "Sidebar"), component("apps/git/diff-pane", "DiffPane"), component("apps/git/help", "GitHelpSheet")],
    async create(context) {
      const { showGitOverlay } = await context.load<typeof GitApp>("packages/tui/src/apps/git/git-tui.ts");
      const files: ChangedFile[] = [{ path: "src/a.ts", kind: "modified", area: "unstaged", additions: 1, deletions: 1 }];
      const model: GitTuiModel = {
        cwd: context.workspace, branch: "fixture-branch", clean: false, unstaged: files, staged: [], headCommit: null,
        refresh: async () => false, loadChangeStats: async () => false, loadHeadFiles: async () => false,
        streamContents: async () => ({ kind: "text", oldText: "const a = 1;\nconst b = 1;\n", newText: "const a = 2;\nconst b = 1;\n", streamResult: undefined as never }),
        stage: async () => context.record("stage", "all"),
        unstage: async () => undefined, discard: async () => undefined,
        commit: async (message, options) => context.record("commit", { message, amend: options?.amend === true }),
        applyPatch: async () => undefined,
      };
      let opened = false;
      const open = () => {
        if (opened) return;
        opened = true;
        queueMicrotask(() => {
          void showGitOverlay(context.tui, { model, createAvatarSource: () => ({ get: () => null }),
            aiStage: async () => { throw new Error("Fixture forbids AI staging"); }, generateCommitMessage: async () => { throw new Error("Fixture forbids model generation"); },
          }).then(() => { context.record("close", true); context.done(); });
        });
      };
      return { render: () => { open(); return []; }, invalidate: () => undefined, describe: () => { open(); return null; } };
    },
    checkpoints: [
      { name: "text diff", comparisons: [{ name: "added line", terminal: { type: "screen", pattern: "(const a = 2;)", group: 1 }, browser: { type: "dom", selector: '[data-kind="added"]', read: "text", pattern: "(const a = 2;)", group: 1 }, expected: "const a = 2;" }] },
      { name: "keyboard shortcuts sheet", terminalInput: "?", browserActions: [{ type: "click", selector: 'button[title^="Keyboard shortcuts"]' }], comparisons: [{ name: "commit shortcut", terminal: { type: "screen", pattern: "(Write the commit message)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.hotkeys.table"] >> nth=-1', read: "text", pattern: "(Write the commit message)", group: 1 }, expected: "Write the commit message" }] },
      { name: "close shortcuts sheet", terminalInput: "?", browserActions: [{ type: "click", selector: 'button[title^="Close"] >> nth=-1' }], comparisons: [visibleText("file", "(src/a\\.ts)", '[data-omp-role="omp.app.git.path"]', "src/a.ts")] },
      { name: "write and commit a message", terminalInput: "cfix: a\x1b[Bsecond line\x1b[B\r", browserActions: [
        { type: "click", selector: '[data-omp-role="omp.app.git.summary"]' },
        { type: "fill", selector: 'input[placeholder="Summary"]', value: "fix: a" },
        { type: "click", selector: '[data-omp-role="omp.app.git.body"]' },
        { type: "fill", selector: 'textarea[placeholder="Description"]', value: "second line" },
        { type: "click", selector: 'button:has-text("Stage all & commit")' },
      ], comparisons: [callbackValue("commit request", "commit", { message: "fix: a\n\nsecond line", amend: false }), callbackValue("stage before commit", "stage", "all")] },
      { name: "close", terminalInput: "q", browserActions: [{ type: "click", selector: 'button[title^="Close"] >> nth=0' }], comparisons: [callbackValue("closed", "close", true)] },
    ],
  },
  {
    id: "app-cleanse-panel-idle-start",
    covers: [component("apps/cleanse-board", "CleanseBoardModel")],
    async create(context) {
      const { CleansePanelComponent } = await context.load<typeof Cleanse>("packages/tui/src/overlays/cleanse-panel.ts");
      const { CleanseBoardModel } = await context.load<typeof Board>("packages/tui/src/apps/cleanse-board.ts");
      const assignment: Board.CleanseAssignment = { index: 0, groups: [{ file: "alpha.ts" }], weight: 1 };
      const panel = new CleansePanelComponent({ model: new CleanseBoardModel(), request: "Fixture repair", tui: context.tui });
      return panelControls(context, panel, { "Start fixture repair": () => { panel.agentStarted("fixture-one", assignment); context.record("started", true); } });
    },
    checkpoints: [
      { name: "idle board", comparisons: [{ name: "request title", terminal: { type: "screen", pattern: "(Fixture repair)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.overlay.cleanse"]', read: "text", pattern: "(Fixture repair)", group: 1 }, expected: "Fixture repair" }] },
      { name: "start repair", terminalInput: "\r", browserActions: [{ type: "click", selector: 'text="Start fixture repair"' }], comparisons: [callbackValue("started", "started", true), { name: "live lane", terminal: { type: "screen", pattern: "(fixture-one)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.cleanse.lane"]', read: "text", pattern: "(fixture-one)", group: 1 }, expected: "fixture-one" }] },
      { name: "close fixture owner", terminalInput: "\x1b", browserActions: [{ type: "press", key: "Escape" }], comparisons: [callbackValue("closed", "closed", true)] },
    ],
  },
  {
    id: "setup-startup-splash-skip",
    covers: [component("setup/startup-splash", "StartupSplashComponent")],
    async create(context) {
      const { runStartupSplash } = await context.load<typeof Splash>("packages/tui/src/setup/startup-splash.ts");
      let shown: Parameters<typeof context.tui.showOverlay>[0] | undefined;
      const ui = {
        terminal: { rows: context.tui.terminal.rows },
        requestRender: () => context.tui.requestRender(),
        invalidate: () => context.tui.invalidate(),
        setFocus: () => undefined,
        showOverlay: (overlay: Parameters<typeof context.tui.showOverlay>[0]) => { shown = overlay; return { hide: () => undefined } as ReturnType<typeof context.tui.showOverlay>; },
      };
      const finished = runStartupSplash({ ui } as Parameters<typeof runStartupSplash>[0], { durationMs: 600_000 });
      if (!shown) throw new Error("The startup splash did not open its overlay");
      void finished.then(() => { context.record("splash-done", true); context.done(); });
      return shown as Awaited<ReturnType<FixtureCase["create"]>>;
    },
    checkpoints: [
      { name: "skip hint", comparisons: [visibleText("hint", "press \\S+ (to skip)", '[data-omp-role="omp.setup.hint"] :text-is("to skip")', "to skip")] },
      { name: "skip the splash", terminalInput: "\r", browserActions: [{ type: "click", selector: '[data-omp-role="omp.setup.splash"]' }], comparisons: [callbackValue("splash finished", "splash-done", true)] },
    ],
  },
];
