import type * as Dashboard from "@omp-source/packages/tui/src/overlays/extensions/extension-dashboard";
import type { Extension } from "@omp-source/packages/tui/src/overlays/extensions/types";
import type { MCPRuntimeSource } from "@omp-source/packages/tui/src/overlays/extensions/mcp-runtime";
import type * as McpWizard from "@omp-source/packages/tui/src/overlays/mcp-add-wizard";
import { callbackValue, visibleText, type FixtureCase } from "../scenario";

const component = (file: string, name: string) => `component:packages/tui/src/overlays/${file}.ts#${name}`;

export const fixtureCases: FixtureCase[] = [
  ...(["extension", "mcp"] as const).map((kind): FixtureCase => ({
    id: `app-extensions-${kind}-toggle`,
    covers: [component("extensions/extension-dashboard", "ExtensionDashboard"), component("extensions/extension-list", "ExtensionList"), component("extensions/inspector-panel", "InspectorPanel")],
    async create(context) {
      const { ExtensionDashboard } = await context.load<typeof Dashboard>("packages/tui/src/overlays/extensions/extension-dashboard.ts");
      const name = kind === "mcp" ? "fixture-mcp" : "Fixture skill";
      const id = kind === "mcp" ? "mcp:fixture-mcp" : "skill:fixture";
      let disabled: string[] = [];
      const extension: Extension = { id, kind: kind === "mcp" ? "mcp" : "skill", name, displayName: name, description: "Inspect deterministic fixture data", path: `${context.workspace}/fixture.json`, source: { provider: "native", providerName: "OMP", level: "project" }, state: "active", raw: kind === "mcp" ? { name, command: "fixture-never-executed", args: [], transport: "stdio", _source: { provider: "native", providerName: "OMP", level: "project", path: `${context.workspace}/fixture.json` } } : { name, description: "Inspect deterministic fixture data" } };
      const mcpSource: MCPRuntimeSource = {
        getConnectionStatus: () => disabled.includes(id) ? "disconnected" : "connected",
        getConnection: () => ({ name: "fixture-mcp", config: { command: "fixture-never-executed", args: [] }, serverInfo: { name: "fixture-mcp", version: "1.0", description: "Fixture MCP tools" }, tools: [{ name: "inspect_fixture", description: "Inspect fixture data", inputSchema: { type: "object", properties: { query: { type: "string" } } } }], resources: [] }),
        getTools: () => [{ mcpServerName: "fixture-mcp", mcpToolName: "inspect_fixture", description: "Inspect fixture data" }],
        getServerResources: () => ({ resources: [], templates: [] }), getServerPrompts: () => [],
      };
      const dashboard = await ExtensionDashboard.create({ terminalHeight: 40, runtime: {
        getDisabledExtensions: () => disabled,
        setDisabledExtensions: values => { disabled = values; context.record("disabled", values); },
        getProviders: () => [{ id: "native", displayName: "OMP", enabled: true, userSourceEnabled: true, foreignUserSource: false }],
        loadExtensions: async values => [{ ...extension, state: values.includes(id) ? "disabled" : "active" }],
        toggleProvider: () => { throw new Error("Provider toggle not admitted by this fixture"); }, toggleUserSource: () => { throw new Error("User source toggle not admitted by this fixture"); },
        persistMcpToggle: async (server, enabled) => { context.record("mcp-persist", { server, enabled }); disabled = enabled ? [] : [id]; },
        applyMcpToggle: async (server, enabled) => context.record("mcp-apply", { server, enabled }), subscribeMcpChanges: () => [],
        mcpSource: kind === "mcp" ? mcpSource : undefined,
      } });
      dashboard.onRequestRender = () => context.tui.requestRender();
      dashboard.onClose = () => { context.record("close", true); context.done(); };
      return dashboard;
    },
    checkpoints: [
      { name: "extension information", comparisons: [visibleText("extension", kind === "mcp" ? "(fixture-mcp)" : "(Fixture skill)", kind === "mcp" ? '[data-omp-picker-item] :text-is("fixture-mcp")' : '[data-omp-picker-item] :text-is("Fixture skill")', kind === "mcp" ? "fixture-mcp" : "Fixture skill")] },
      { name: "select extension row", terminalInput: "\x1b[B", browserActions: [{ type: "click", selector: kind === "mcp" ? '[data-omp-picker-item]:has-text("fixture-mcp")' : '[data-omp-picker-item]:has-text("Fixture skill")' }], comparisons: [visibleText("selected extension", kind === "mcp" ? "(fixture-mcp)" : "(Fixture skill)", kind === "mcp" ? '[data-omp-picker-item] :text-is("fixture-mcp")' : '[data-omp-picker-item] :text-is("Fixture skill")', kind === "mcp" ? "fixture-mcp" : "Fixture skill")] },
      { name: "disable selected extension", terminalInput: " ", browserActions: [{ type: "click", selector: 'button:has-text("Toggle")' }], comparisons: kind === "mcp" ? [callbackValue("MCP persistence", "mcp-persist", { server: "fixture-mcp", enabled: false }), callbackValue("MCP live application", "mcp-apply", { server: "fixture-mcp", enabled: false })] : [callbackValue("disabled extension IDs", "disabled", ["skill:fixture"])] },
      { name: "close", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button:text-is("Close") >> nth=0' }], comparisons: [callbackValue("closed", "close", true)] },
    ],
  })),
  {
    id: "app-extensions-search-empty",
    covers: [component("extensions/extension-dashboard", "ExtensionDashboard"), component("extensions/extension-list", "ExtensionList"), component("extensions/inspector-panel", "InspectorPanel")],
    async create(context) {
      const { ExtensionDashboard } = await context.load<typeof Dashboard>("packages/tui/src/overlays/extensions/extension-dashboard.ts");
      const extension: Extension = { id: "skill:fixture", kind: "skill", name: "Fixture skill", displayName: "Fixture skill", description: "Inspect deterministic fixture data", path: `${context.workspace}/fixture.json`, source: { provider: "native", providerName: "OMP", level: "project" }, state: "active", raw: { name: "Fixture skill", description: "Inspect deterministic fixture data" } };
      const dashboard = await ExtensionDashboard.create({ terminalHeight: 40, runtime: {
        getDisabledExtensions: () => [], setDisabledExtensions: () => undefined,
        getProviders: () => [{ id: "native", displayName: "OMP", enabled: true, userSourceEnabled: true, foreignUserSource: false }],
        loadExtensions: async () => [extension],
        toggleProvider: () => { throw new Error("Provider toggle not admitted by this fixture"); }, toggleUserSource: () => { throw new Error("User source toggle not admitted by this fixture"); },
        persistMcpToggle: async () => undefined, applyMcpToggle: async () => undefined, subscribeMcpChanges: () => [], mcpSource: undefined,
      } });
      dashboard.onRequestRender = () => context.tui.requestRender();
      dashboard.onClose = () => { context.record("close", true); context.done(); };
      return dashboard;
    },
    checkpoints: [
      { name: "listed extension", comparisons: [visibleText("extension", "(Fixture skill)", '[data-omp-picker-item] :text-is("Fixture skill")', "Fixture skill")] },
      { name: "search without a match", terminalInput: "zzz", browserActions: [{ type: "fill", selector: 'input[placeholder="Search extensions…"]', value: "zzz" }], comparisons: [
        visibleText("empty list", "(No extensions found for this provider\\.)", 'text="No extensions found for this provider."', "No extensions found for this provider."),
        { name: "inspector prompt", terminal: { type: "screen", pattern: "(Select an extension)", group: 1 }, browser: { type: "dom", selector: 'text="Select an extension to view details"', read: "text", pattern: "(Select an extension)", group: 1 }, expected: "Select an extension" },
      ] },
      { name: "clear search", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button:text-is("Clear search")' }], comparisons: [visibleText("restored extension", "(Fixture skill)", '[data-omp-picker-item] :text-is("Fixture skill")', "Fixture skill")] },
      { name: "close", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button:text-is("Close") >> nth=0' }], comparisons: [callbackValue("closed", "close", true)] },
    ],
  },
  ...(["invalid-cancel", "commit"] as const).map((mode): FixtureCase => ({
    id: `dialog-mcp-add-${mode}`,
    covers: [component("mcp-add-wizard", "MCPAddWizard")],
    async create(context) {
      const { MCPAddWizard } = await context.load<typeof McpWizard>("packages/tui/src/overlays/mcp-add-wizard.ts");
      return new MCPAddWizard({ validateServerName: name => /^[\w.:-]+$/.test(name) ? undefined : "Use letters, numbers, dash or underscore", analyzeAuthError: () => ({ requiresAuth: false }), discoverOAuthEndpoints: async () => null, fetchResourceMetadataScopes: async () => undefined }, (name, config, scope) => { context.record("server", { name, config, scope }); context.done(); }, () => { context.record("cancel", true); context.done(); }, undefined, undefined, () => context.tui.requestRender(), mode === "commit" ? "fixture-mcp" : undefined);
    },
    checkpoints: mode === "invalid-cancel" ? [
      { name: "name entry", comparisons: [visibleText("title", "(Name)", '[data-omp-role="omp.mcpAdd.phase"]:text-is("Name")', "Name")] },
      { name: "invalid name stays in field", terminalInput: "bad name\r", browserActions: [{ type: "fill", selector: '[data-omp-role="omp.mcpAdd.field"] input', value: "bad name" }, { type: "click", selector: 'button:has-text("Continue")' }], comparisons: [visibleText("validation error", "(Use letters, numbers, dash or underscore)", '[data-omp-role="omp.mcpAdd.note"]', "Use letters, numbers, dash or underscore")] },
      { name: "cancel", terminalInput: "\x03", browserActions: [{ type: "click", selector: 'button:has-text("Cancel")' }], comparisons: [callbackValue("cancelled", "cancel", true)] },
    ] : [
      { name: "transport choices", comparisons: [visibleText("transport", "(stdio)", '[data-omp-role="omp.mcpAdd.choices"] :text-is("stdio")', "stdio")] },
      { name: "choose stdio", terminalInput: "\r", browserActions: [{ type: "click", selector: 'button:has-text("Continue")' }], comparisons: [visibleText("command step", "(Command)", '[data-omp-role="omp.mcpAdd.title"]', "Command")] },
      { name: "command", terminalInput: "fixture-never-executed\r", browserActions: [{ type: "fill", selector: '[data-omp-role="omp.mcpAdd.field"] input', value: "fixture-never-executed" }, { type: "click", selector: 'button:has-text("Continue")' }], comparisons: [visibleText("arguments step", "(Arguments)", '[data-omp-role="omp.mcpAdd.title"]', "Arguments")] },
      { name: "arguments", terminalInput: "--fixture\r", browserActions: [{ type: "fill", selector: '[data-omp-role="omp.mcpAdd.field"] input', value: "--fixture" }, { type: "click", selector: 'button:has-text("Continue")' }], comparisons: [visibleText("scope choice", "(This project)", '[data-omp-role="omp.mcpAdd.choices"] :text-is("This project")', "This project")] },
      { name: "project scope", terminalInput: "\x1b[B\r", browserActions: [{ type: "click", selector: '[data-omp-role="omp.mcpAdd.choices"] :text-is("This project")' }, { type: "click", selector: 'button:has-text("Continue")' }], comparisons: [visibleText("reviewed command", "(fixture-never-executed)", '[data-omp-role="omp.mcpAdd.summary"] :text-is("fixture-never-executed")', "fixture-never-executed")] },
      { name: "save", terminalInput: "\r", browserActions: [{ type: "click", selector: 'button:has-text("Save server")' }], comparisons: [callbackValue("saved server", "server", { name: "fixture-mcp", config: { type: "stdio", command: "fixture-never-executed", args: ["--fixture"] }, scope: "project" })] },
    ],
  })),
];
