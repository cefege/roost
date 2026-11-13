import type * as LogViewer from "@omp-source/packages/tui/src/apps/debug/log-viewer";
import type * as RawViewer from "@omp-source/packages/tui/src/apps/debug/raw-sse";
import type * as RawBuffer from "@omp-source/packages/tui/src/apps/debug/raw-sse-buffer";
import type * as PauseScreen from "@omp-source/packages/tui/src/overlays/pause-screen";
import type * as JobsPanel from "@omp-source/packages/tui/src/overlays/jobs-panel";
import type * as AgentsHub from "@omp-source/packages/tui/src/overlays/agents-hub";
import type * as SessionInfo from "@omp-source/packages/tui/src/overlays/session-info-overlay";
import type { ModelBrowserSource } from "@omp-source/packages/tui/src/overlays/model-browser";
import { callbackValue, visibleText, type FixtureCase } from "../scenario";

const component = (file: string, name: string) => `component:packages/tui/src/${file}.ts#${name}`;
const browserSource: ModelBrowserSource = {
  revision: 0, defaultThinkingLevel: "off", modelProviderOrder: [], knownRoleIds: [], mruOrder: [], modelPerf: new Map(),
  getRoleInfo: role => ({ name: role, section: "chat", accepts: () => true }), getModelRole: () => undefined,
  defaultRoleChain: () => [], resolveRoleValue: () => ({ model: undefined, explicitThinkingLevel: false }),
};
const fullscreenPresentation = { overlay: true, overlayOptions: { anchor: "top-left", width: "100%", maxHeight: "100%", margin: 0, fullscreen: true } } as const;

export const fixtureCases: FixtureCase[] = [
  {
    id: "app-debug-log-filter-copy",
    covers: [component("apps/debug/log-viewer", "DebugLogViewerComponent")],
    presentation: fullscreenPresentation,
    async create(context) {
      const { DebugLogViewerComponent } = await context.load<typeof LogViewer>("packages/tui/src/apps/debug/log-viewer.ts");
      return new DebugLogViewerComponent({
        logs: "Fixture frame accepted\nFixture acknowledgement refused", terminalRows: 40, processStartMs: 0,
        deps: { copyToClipboard: text => context.record("copy", text) },
        onExit: () => { context.record("close", true); context.done(); }, onUpdate: () => context.tui.requestRender(),
      });
    },
    checkpoints: [
      { name: "log information", comparisons: [visibleText("entry", "(Fixture frame accepted)", '[data-omp-picker-item="l0"] :text-is("Fixture frame accepted")', "Fixture frame accepted")] },
      { name: "filter", terminalInput: "refused", browserActions: [{ type: "fill", selector: 'input[placeholder="Filter logs…"]', value: "refused" }], comparisons: [visibleText("matching entry", "(Fixture acknowledgement refused)", '[data-omp-picker-item="l1"] :text-is("Fixture acknowledgement refused")', "Fixture acknowledgement refused")] },
      { name: "copy selected real log", terminalInput: "\x03", browserActions: [{ type: "click", selector: 'button:has-text("Copy") >> nth=0' }], comparisons: [callbackValue("clipboard payload", "copy", "Fixture acknowledgement refused")] },
      { name: "close", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button:has-text("Close") >> nth=0' }], comparisons: [callbackValue("closed", "close", true)] },
    ],
  },
  {
    id: "app-debug-raw-stream-copy",
    covers: [component("apps/debug/raw-sse", "RawSseViewerComponent")],
    presentation: fullscreenPresentation,
    async create(context) {
      const { RawSseViewerComponent } = await context.load<typeof RawViewer>("packages/tui/src/apps/debug/raw-sse.ts");
      const { RawSseDebugBuffer } = await context.load<typeof RawBuffer>("packages/tui/src/apps/debug/raw-sse-buffer.ts");
      const buffer = new RawSseDebugBuffer();
      buffer.recordEvent({ event: "message", data: "fixture-frame", raw: ["data: fixture-frame"] });
      return new RawSseViewerComponent({ buffer, terminalRows: 40,
        deps: { copyToClipboard: text => context.record("copy", text) },
        onExit: () => { context.record("close", true); context.done(); }, onUpdate: () => context.tui.requestRender(),
      });
    },
    checkpoints: [
      { name: "raw event information", comparisons: [visibleText("frame", "(data: fixture-frame)", '[data-omp-role="omp.debug.stream"] :text-is("data: fixture-frame")', "data: fixture-frame")] },
      { name: "copy raw", terminalInput: "\x03", browserActions: [{ type: "click", selector: 'button:has-text("Copy raw")' }], comparisons: [callbackValue("raw clipboard payload", "copy", "data: fixture-frame\n")] },
      { name: "close", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button:has-text("Close") >> nth=0' }], comparisons: [callbackValue("closed", "close", true)] },
    ],
  },
  {
    id: "app-debug-raw-stream-empty",
    covers: [component("apps/debug/raw-sse", "RawSseViewerComponent")],
    presentation: fullscreenPresentation,
    async create(context) {
      const { RawSseViewerComponent } = await context.load<typeof RawViewer>("packages/tui/src/apps/debug/raw-sse.ts");
      const { RawSseDebugBuffer } = await context.load<typeof RawBuffer>("packages/tui/src/apps/debug/raw-sse-buffer.ts");
      return new RawSseViewerComponent({ buffer: new RawSseDebugBuffer(), terminalRows: 40,
        deps: { copyToClipboard: text => context.record("copy", text) },
        onExit: () => { context.record("close", true); context.done(); }, onUpdate: () => context.tui.requestRender(),
      });
    },
    checkpoints: [
      { name: "empty stream notice", comparisons: [
        visibleText("title", "(No raw SSE frames captured yet\\.)", '[data-omp-role="omp.app.empty-title"]', "No raw SSE frames captured yet."),
        visibleText("explanation", "(HTTP SSE providers populate this view while a model response is streaming\\.)", '[data-omp-role="omp.app.empty"] :text-is("HTTP SSE providers populate this view while a model response is streaming.")', "HTTP SSE providers populate this view while a model response is streaming."),
      ] },
      { name: "copy with nothing captured", terminalInput: "\x03", browserActions: [{ type: "click", selector: 'button:has-text("Copy raw")' }], comparisons: [visibleText("status", "(No raw SSE frames to copy)", '[data-omp-role="omp.app.status"]', "No raw SSE frames to copy")] },
      { name: "close", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button:has-text("Close") >> nth=0' }], comparisons: [callbackValue("closed", "close", true)] },
    ],
  },
  ...(["settled", "outstanding"] as const).map((state): FixtureCase => ({
    id: `app-pause-${state}-resume`,
    covers: [component("overlays/pause-screen", "PauseScreenComponent")],
    async create(context) {
      const { PauseScreenComponent } = await context.load<typeof PauseScreen>("packages/tui/src/overlays/pause-screen.ts");
      const screen = new PauseScreenComponent({ ui: context.tui, sessionName: "Fixture session", showStatus: text => context.record("status", text) }, { getWorkStatus: () => ({ unsettledTurns: state === "outstanding" ? 2 : 0, unfinishedJobs: state === "outstanding" ? 1 : 0 }) });
      void screen.run().then(() => { screen.dispose(); context.record("resume", true); context.done(); });
      return screen;
    },
    checkpoints: [
      { name: "settlement state", comparisons: [visibleText("status", state === "settled" ? "(Paused — ready to suspend)" : "(Pausing — waiting for work to settle)", '[data-omp-role="omp.pause.settlement"]', state === "settled" ? "Paused — ready to suspend" : "Pausing — waiting for work to settle")] },
      { name: "resume", terminalInput: "\r", browserActions: [{ type: "click", selector: 'button:has-text("Resume")' }], comparisons: [callbackValue("resumed", "resume", true)] },
    ],
  })),
  {
    id: "app-jobs-cancel-output",
    covers: [component("overlays/jobs-panel", "JobsSheet")],
    async create(context) {
      const { JobsSheet } = await context.load<typeof JobsPanel>("packages/tui/src/overlays/jobs-panel.ts");
      const job: JobsPanel.JobsPanelJob = { id: "fixture-job", type: "bash", label: "printf fixture", intent: "Inspect fixture output", caption: "Prints one fixture line", status: "running", startTime: 1794000000000 };
      return new JobsSheet({
        load: () => ({ running: job.status === "running" ? [job] : [], recent: job.status === "running" ? [] : [job] }),
        inspect: () => ({ command: "printf fixture", cwd: context.workspace, pids: [], output: "Fixture tool output", widget: { title: "fixture-bench", stage: "Warming fixture cache", blocks: [], state: "fallback", updatedAt: 1794000000000 } }),
        cancel: id => { context.record("cancel-job", id); Object.assign(job, { status: "cancelled" }); context.tui.requestRender(); },
        close: () => { context.record("close", true); context.done(); },
      });
    },
    checkpoints: [
      { name: "tool output", comparisons: [visibleText("output", "(Fixture tool output)", 'text="Fixture tool output"', "Fixture tool output"), visibleText("caption", "(Prints one fixture line)", 'text="Prints one fixture line"', "Prints one fixture line"), visibleText("widget stage", "(Warming fixture cache)", 'text="Warming fixture cache"', "Warming fixture cache")] },
      { name: "cancel actual selected job", terminalInput: "x", browserActions: [{ type: "click", selector: 'button:has-text("Cancel job")' }], comparisons: [callbackValue("cancelled job", "cancel-job", "fixture-job")] },
      { name: "close", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button[title^="Close"]' }], comparisons: [callbackValue("closed", "close", true)] },
    ],
  },
  {
    id: "app-jobs-empty-close",
    covers: [component("overlays/jobs-panel", "JobsSheet")],
    async create(context) {
      const { JobsSheet } = await context.load<typeof JobsPanel>("packages/tui/src/overlays/jobs-panel.ts");
      return new JobsSheet({ load: () => ({ running: [], recent: [] }), inspect: () => undefined, close: () => { context.record("close", true); context.done(); } });
    },
    checkpoints: [
      { name: "empty state", comparisons: [visibleText("empty title", "(No background jobs)", 'text="No background jobs"', "No background jobs")] },
      { name: "close", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button[title^="Close"]' }], comparisons: [callbackValue("closed", "close", true)] },
    ],
  },
  {
    id: "app-agents-toggle-and-cancel",
    covers: [component("overlays/agents-hub", "AgentsHubComponent")],
    async create(context) {
      const { AgentsHubComponent } = await context.load<typeof AgentsHub>("packages/tui/src/overlays/agents-hub.ts");
      const agent: AgentsHub.HubAgent = { name: "FixtureScout", description: "Read only fixture research", systemPrompt: "Inspect fixture data", source: "project", disabled: false };
      return AgentsHubComponent.create(context.tui, {
        browserSource, loadAgents: async () => [agent], getAvailableModels: () => [], effectiveModelPatterns: () => [], resolvePatterns: () => undefined,
        effectivePrewalkPattern: () => undefined, effectiveAdvisorPattern: () => undefined,
        setAgentDisabled: (name, options) => { agent.disabled = options.disabled; context.record("disabled", { name, disabled: options.disabled }); },
        setAgentOverride: (property, name, value) => context.record("override", { property, name, value }),
        generateAgent: async () => { throw new Error("Fixture forbids model generation"); },
        saveAgent: async () => { throw new Error("Fixture forbids writes outside its constructor state"); },
      }, { onCancel: () => { context.record("cancel", true); context.done(); } });
    },
    checkpoints: [
      { name: "agent information", comparisons: [visibleText("agent", "(FixtureScout)", '[data-omp-picker-item="agent:project:FixtureScout"] :text-is("FixtureScout")', "FixtureScout")] },
      { name: "disable", terminalInput: " ", browserActions: [{ type: "click", selector: 'button:has-text("Toggle")' }], comparisons: [callbackValue("disabled state", "disabled", { name: "FixtureScout", disabled: true })] },
      { name: "cancel", terminalInput: "\x1b", browserActions: [{ type: "press", key: "Escape" }], comparisons: [callbackValue("cancelled", "cancel", true)] },
    ],
  },
  {
    id: "app-session-info-close",
    covers: [component("overlays/session-info-overlay", "SessionInfoOverlay")],
    async create(context) {
      const { SessionInfoOverlay } = await context.load<typeof SessionInfo>("packages/tui/src/overlays/session-info-overlay.ts");
      const dim = (label: string) => context.theme.fg("dim", label);
      const heading = (label: string) => context.theme.bold(label);
      const report = [
        `${dim("File:")} ${context.workspace}/fixture-session.jsonl`, `${dim("ID:")} fixture-session-id`, "",
        heading("Provider"), `${dim("Model:")} parity/alpha`, `${dim("Append-Only:")} inactive`, "",
        heading("Messages"), `${dim("User:")} 2`, `${dim("Assistant:")} 3`, `${dim("Tool Calls:")} 4`, `${dim("Total:")} 9`, "",
        heading("Tokens"), `${dim("Input:")} 1,204`, `${dim("Total:")} 1,716`, "",
        heading("Cost"), `${dim("Total:")} 0.0412`, "",
        heading("MCP Servers"), `${dim("None connected")}`,
      ].join("\n");
      return new SessionInfoOverlay(context.tui, report, () => { context.record("close", true); context.done(); });
    },
    checkpoints: [
      { name: "session report", comparisons: [
        visibleText("title", "(Session Info)", '[role="dialog"] h3 span:text-is("Session Info")', "Session Info"),
        visibleText("session id", "(fixture-session-id)", 'text="fixture-session-id"', "fixture-session-id"),
        visibleText("model", "(parity/alpha)", 'text="parity/alpha"', "parity/alpha"),
      ] },
      { name: "close", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button:text-is("Close") >> nth=0' }], comparisons: [callbackValue("closed", "close", true)] },
    ],
  },
];
