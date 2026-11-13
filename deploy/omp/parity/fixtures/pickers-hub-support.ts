//! Builds the real model hub overlay over a fixed three-model catalogue for the picker proof scenarios.
//! Owned by pickers-hub.ts; depends on the pinned OMP source loaded through the fixture context.
import type * as CatalogBuild from "@omp-source/packages/catalog/src/build";
import type * as ModelHub from "@omp-source/packages/tui/src/overlays/model-hub";
import type { FixtureContext } from "../scenario";

const overlays = "packages/tui/src/overlays/";
const catalogue = [
  { provider: "parity", id: "alpha", name: "Fixture alpha", reasoning: true, cost: 3 },
  { provider: "parity", id: "beta", name: "Fixture beta", reasoning: false, cost: 0 },
  { provider: "other", id: "gamma", name: "Fixture gamma", reasoning: false, cost: 1 },
];

export interface ModelHubOptions {
  /** Catalogue size beyond the three base models, so one role move patches rather than resends the rows. */
  extraModels?: number;
  /** Provider/id removed from the registry when a role assignment lands, as a provider refresh would. */
  dropOnAssign?: string;
  /** False leaves the hub unscoped so the registry, not a fixed `--models` list, decides the rows. */
  scoped?: boolean;
  /** Providers whose models sit in the catalogue but need sign-in, so their sidebar scope is locked. */
  lockedProviders?: string[];
  /** Discovery status per available provider; each one shows as that provider scope's status dot. */
  discovery?: Record<string, "ok" | "cached" | "unavailable" | "unauthenticated">;
}

export async function createModelHub(context: FixtureContext, options: ModelHubOptions = {}) {
  const { ModelHubComponent } = await context.load<typeof ModelHub>(`${overlays}model-hub.ts`);
  const { buildModel } = await context.load<typeof CatalogBuild>("packages/catalog/src/build.ts");
  const extras = Array.from({ length: options.extraModels ?? 0 }, (_, index) => ({ provider: "parity", id: `extra-${String(index + 1).padStart(2, "0")}`, name: `Fixture extra ${index + 1}`, reasoning: false, cost: 1 }));
  const models = [...catalogue, ...extras].map(spec => buildModel({
    provider: spec.provider, id: spec.id, name: spec.name, api: "openai-completions", baseUrl: "http://127.0.0.1:1/v1", reasoning: spec.reasoning, input: ["text", "image"],
    cost: { input: spec.cost, output: spec.cost * 2, cacheRead: 0, cacheWrite: 0 }, contextWindow: 32000, maxTokens: 4096,
  }));
  const lockedModels = (options.lockedProviders ?? []).map(provider => buildModel({
    provider, id: "sealed", name: "Fixture sealed", api: "openai-completions", baseUrl: "http://127.0.0.1:1/v1", reasoning: false, input: ["text"],
    cost: { input: 1, output: 2, cacheRead: 0, cacheWrite: 0 }, contextWindow: 32000, maxTokens: 4096,
  }));
  const roles: Record<string, string> = { default: "parity/alpha", smol: "other/gamma" };
  const source: ModelHub.ModelHubSource = {
    revision: 0, defaultThinkingLevel: "off", modelProviderOrder: [], knownRoleIds: ["default", "smol"], mruOrder: [], modelPerf: new Map(),
    disabledProviders: [], fallbackChains: {}, modelRoleStorage: "global", cycleOrder: [],
    getRoleInfo: role => ({ tag: role, name: role, section: "chat", accepts: () => true }), defaultRoleChain: () => [],
    getModelRole: role => roles[role], getProjectModelRole: () => undefined, getGlobalModelRole: role => roles[role], getModelRoleSource: () => "global",
    resolveRoleValue: value => ({ model: models.find(model => `${model.provider}/${model.id}` === value), explicitThinkingLevel: false }),
  };
  return new ModelHubComponent(context.tui, source, {
    getError: () => undefined, getAvailable: () => models, getAll: () => [...models, ...lockedModels], find: (provider, id) => models.find(model => model.provider === provider && model.id === id),
    authStorage: { keys: { source: () => undefined } }, getDiscoverableProviders: () => [], getProviderDiscoveryState: provider => options.discovery?.[provider] ? { status: options.discovery[provider], optional: false } : undefined,
    refresh: async () => undefined, refreshProvider: async () => undefined,
  }, options.scoped === false ? [] : models.map(model => ({ model })), {
    onAssign: async (_model, role, _level, selector) => {
      roles[role] = selector;
      context.record("assign", { role, selector });
      const dropped = models.findIndex(model => `${model.provider}/${model.id}` === options.dropOnAssign);
      if (dropped >= 0) models.splice(dropped, 1);
    },
    onUnassign: () => undefined, onCancel: () => context.done(),
  }, { currentSelector: "parity/alpha" });
}
