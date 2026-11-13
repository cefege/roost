import { writeFile } from "node:fs/promises";
import type * as CatalogBuild from "@omp-source/packages/catalog/src/build";
import type * as ModelHub from "@omp-source/packages/tui/src/overlays/model-hub";
import { callbackValue, visibleText, type FixtureCase } from "../scenario";
import { pickerQueryProof } from "./pickers";

export const fixtureCases: FixtureCase[] = (["assign", "cancel"] as const).map((mode): FixtureCase => ({
  id: `app-model-hub-${mode}`,
  covers: ["component:packages/tui/src/overlays/model-hub.ts#ModelHubComponent", "component:packages/tui/src/overlays/model-browser.ts#ModelBrowser", ...(mode === "assign" ? ["property:TspPickerProps.query"] : [])],
  behaviors: ["picker:model-role-assignment", `picker:${mode}`],
  async create(context) {
    const { ModelHubComponent } = await context.load<typeof ModelHub>("packages/tui/src/overlays/model-hub.ts");
    const { buildModel } = await context.load<typeof CatalogBuild>("packages/catalog/src/build.ts");
    const models = ["alpha", "beta"].map(id => buildModel({ provider: "parity", id, name: `Fixture ${id}`, api: "openai-completions", baseUrl: "http://127.0.0.1:1/v1", reasoning: false, input: ["text"], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 32000, maxTokens: 4096 }));
    const roles: Record<string, string> = { default: "parity/alpha" };
    const source: ModelHub.ModelHubSource = {
      revision: 0, defaultThinkingLevel: "off", modelProviderOrder: [], knownRoleIds: ["default", "smol"], mruOrder: [], modelPerf: new Map(),
      disabledProviders: [], fallbackChains: {}, modelRoleStorage: "global", cycleOrder: [],
      getRoleInfo: role => ({ tag: role, name: role, section: "chat", accepts: () => true }), defaultRoleChain: () => [],
      getModelRole: role => roles[role], getProjectModelRole: () => undefined, getGlobalModelRole: role => roles[role], getModelRoleSource: () => "global",
      resolveRoleValue: value => ({ model: models.find(model => `${model.provider}/${model.id}` === value), explicitThinkingLevel: false }),
    };
    return new ModelHubComponent(context.tui, source, {
      getError: () => undefined, getAvailable: () => models, getAll: () => models, find: (provider, id) => models.find(model => model.provider === provider && model.id === id),
      authStorage: { keys: { source: () => undefined } }, getDiscoverableProviders: () => [], getProviderDiscoveryState: () => undefined,
      refresh: async () => undefined, refreshProvider: async () => undefined,
    }, models.map(model => ({ model })), {
      onAssign: async (_model, role, _level, selector) => { roles[role] = selector; await writeFile(`${context.workspace}/role-selection.json`, JSON.stringify(roles)); context.record("assignment", { role, selector }); context.done(); },
      onUnassign: role => { delete roles[role]; context.record("unassign", role); }, onCancel: () => { context.record("cancel", true); context.done(); },
    }, { currentSelector: "parity/alpha" });
  },
  checkpoints: [
    { name: "real model browser", comparisons: [visibleText("current model", "(Fixture alpha)", 'text="Fixture alpha"', "Fixture alpha")] },
    ...(mode === "assign" ? [
      { name: "search available model", terminalInput: "beta", browserActions: [{ type: "fill" as const, selector: 'input[placeholder="Search models…"]', value: "beta" }], comparisons: [visibleText("matching model", "(Fixture beta)", 'text="Fixture beta"', "Fixture beta"), pickerQueryProof("beta", 'input[placeholder="Search models…"]')] },
      { name: "open role assignment", terminalInput: "\r", browserActions: [{ type: "press" as const, key: "Enter", selector: 'input[placeholder="Search models…"]' }], comparisons: [visibleText("role choice", "(smol)", 'button:text-is("smol")', "smol")] },
      { name: "persist role assignment", terminalInput: "\x1b[C\r", browserActions: [{ type: "click" as const, selector: 'button:text-is("smol")' }], comparisons: [callbackValue("assigned", "assignment", { role: "smol", selector: "parity/beta" }), { name: "persisted role", terminal: { type: "file" as const, path: "role-selection.json", pointer: "/smol" }, browser: { type: "file" as const, path: "role-selection.json", pointer: "/smol" }, expected: "parity/beta" }] },
    ] : [{ name: "cancel", terminalInput: "\x1b", browserActions: [{ type: "press" as const, key: "Escape" }], comparisons: [callbackValue("cancelled", "cancel", true)] }]),
  ],
}));
