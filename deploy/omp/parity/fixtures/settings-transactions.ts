import { mkdir } from "node:fs/promises";
import type { FixtureCase } from "../scenario";
import { callbackValue, visibleText } from "../scenario";
import { getSettingsForTab, SETTING_TABS } from "@omp-source/packages/tui/src/overlays/settings-defs";
import { attribute, caseId, createSettings, effects, escapePattern, selectedLabel, settingDefinitions } from "./settings-support";
import { settingRow } from "./settings-proofs";

const disabledSeeds: Record<string, Record<string, unknown>> = {
  advisorEnabled: { "advisor.enabled": false }, vimModeEnabled: { "tui.vimMode": false },
  composerPredictionsEnabled: { "composer.predictions": false }, hindsightActive: { "memory.backend": "off" },
  mnemopiActive: { "memory.backend": "off" }, autolearnActive: { "autolearn.enabled": false },
  autoThinkingActive: { defaultThinkingLevel: "high" }, usageAwareFallbackEnabled: { "retry.usageAwareFallback": false },
  planModeEnabled: { "plan.enabled": false }, planAutosaveEnabled: { "plan.enabled": true, "plan.autosave": false },
  macOS: {}, hasImageProtocol: {}, nativeRendering: {},
  contextFilesExtraNotProject: {}, restrictedFoldersNotProject: {}, perCallModelAllowNotProject: {},
};
const projectConditions: Record<string, true> = { contextFilesExtraNotProject: true, restrictedFoldersNotProject: true, perCallModelAllowNotProject: true };
export const conditionFixtureCases: FixtureCase[] = settingDefinitions.flatMap(({ setting, definition }) => {
  const condition = setting.ui?.condition;
  if (!condition) return [];
  if (!(condition in disabledSeeds)) throw new Error(`Missing settings condition fixture: ${condition}`);
  return [{ id: `settings-${caseId(definition.path)}-condition-hidden`, covers: [`setting-condition:${condition}:false`, `setting-condition:${definition.path}:false`],
    async create(context) {
      if (projectConditions[condition]) {
        const projectValue = definition.path === "contextFiles.extra" ? ["TEAM.md"] : definition.path === "task.perCallModelAllow" ? ["alpha/model"] : ["private"];
        const config: Record<string, unknown> = {};
        let cursor = config;
        const segments = definition.path.split(".");
        for (const segment of segments.slice(0, -1)) { const child: Record<string, unknown> = {}; cursor[segment] = child; cursor = child; }
        const leaf = segments.at(-1);
        if (!leaf) throw new Error("Empty setting path");
        cursor[leaf] = projectValue;
        await mkdir(`${context.workspace}/.omp`, { recursive: true });
        await Bun.write(`${context.workspace}/.omp/settings.json`, JSON.stringify(config));
        const { Settings } = await context.load<typeof import("@omp-source/packages/coding-agent/src/config/settings")>("packages/coding-agent/src/config/settings.ts");
        await Settings.instance.reloadForCwd(context.workspace);
        await Settings.instance.reloadFromDisk();
      }
      return createSettings(context, definition, disabledSeeds[condition], { hidden: true, capabilities: { macOS: condition !== "macOS", hasImageProtocol: condition !== "hasImageProtocol", nativeRendering: condition !== "nativeRendering" } });
    }, checkpoints: [{ name: "condition hides actual row", comparisons: [
      { name: "settings owner remains visible", terminal: { type: "screen", pattern: "(Settings)", read: "visible" }, browser: { type: "dom", selector: "[data-omp-page]", read: "visible" }, expected: true },
      callbackValue("canonical visibility", "visibility", false),
      ...(projectConditions[condition] ? [callbackValue("project provenance", "provenance", "project")] : []),
      { name: "row absent", terminal: { type: "screen", pattern: escapePattern(definition.label), read: "visible" }, browser: { type: "dom", selector: settingRow(definition.path), read: "visible" }, expected: false,
        proves: [`setting-condition:${condition}:false`, `setting-condition:${definition.path}:false`].map(entry => ({ entry, effect: "interaction" as const, value: false,
          target: { checkpoint: "condition hides actual row", comparison: "settings owner remains visible" }, assertionGroups: [{ comparisons: [
            { checkpoint: "condition hides actual row", comparison: "canonical visibility" }, { checkpoint: "condition hides actual row", comparison: "row absent" },
          ] }] })) },
    ] }],
  }];
});
export const tabFixtureCases: FixtureCase[] = SETTING_TABS.map((tab, index) => {
  const definitions = getSettingsForTab(settingDefinitions.map(({ setting }) => ({ path: setting.id, type: setting.type, defaultValue: setting.default,
    ui: setting.ui, enumValues: setting.enumValues, credential: setting.isCredential })), tab);
  const firstRow = definitions.find(definition => !settingDefinitions.find(entry => entry.definition.path === definition.path)?.setting.ui?.condition);
  if (!firstRow) throw new Error(`No unconditional visible source row for tab ${tab}`);
  return { id: `settings-tab-${tab}`, covers: [`settings-tab:${tab}`, "property:TspPrefsRow.label"],
    create: context => createSettings(context), checkpoints: [{ name: "visit actual tab", terminalInput: index ? "\x1b[C".repeat(index) : "\x1b[C\x1b[D",
      browserActions: [...(index ? [] : [{ type: "click" as const, selector: "[data-omp-page-tab=model] button" }]), { type: "click", selector: `[data-omp-page-tab=${attribute(tab)}] button` }],
      comparisons: [selectedLabel(firstRow), {
        name: "active tab owns displayed source row", terminal: { type: "screen", pattern: escapePattern(firstRow.label), read: "visible" },
        browser: { type: "dom", selector: `[data-omp-page=${attribute(tab)}] ${settingRow(firstRow.path)}`, read: "visible" }, expected: true,
        proves: [{ entry: `settings-tab:${tab}`, effect: "interaction", value: tab, assertionGroups: [{ comparisons: [{ checkpoint: "visit actual tab", comparison: "active tab owns displayed source row" }] }] }],
      }] }],
  };
});
const providerDefinition = settingDefinitions.find(entry => entry.definition.type === "providerLimits")?.definition;
if (!providerDefinition) throw new Error("Canonical provider-limit editor missing");
export const providerLimitsFixtureCases: FixtureCase[] = [{ id: "settings-provider-limits-transaction", covers: ["setting:providers.maxInFlightRequests", "control:providerLimits", "settings:providerLimits:validation", "settings:providerLimits:rounding", "settings:providerLimits:clear", "component:packages/tui/src/overlays/settings-selector.ts#ProviderLimitsSubmenu"],
  create: context => createSettings(context, providerDefinition, { "providers.maxInFlightRequests": {} }), checkpoints: [
    { name: "provider row", comparisons: [selectedLabel(providerDefinition)] },
    { name: "open provider editor", terminalInput: "\r\r", browserActions: [{ type: "click", selector: `${settingRow(providerDefinition.path)} button` }, { type: "click", selector: "[data-omp-role='omp.select'] button:has-text('alpha')" }], comparisons: [visibleText("provider editing", "(Max In-Flight Requests: alpha)", "[data-omp-role='omp.prefs.editor'] :text-is('Max In-Flight Requests: alpha')", "Max In-Flight Requests: alpha")] },
    { name: "reject nonpositive", terminalInput: "0\r", browserActions: [{ type: "fill", selector: "[data-omp-role='omp.prefs.editor'] input", value: "0" }, { type: "press", selector: "[data-omp-role='omp.prefs.editor'] input", key: "Enter" }], comparisons: [visibleText("validation", "(Limit must be a positive number\\.)", "[data-omp-role='omp.prefs.editor'] :text-is('Limit must be a positive number.')", "Limit must be a positive number.")] },
    { name: "round decimal and persist", terminalInput: "\x01\x0b2.9\r", browserActions: [{ type: "fill", selector: "[data-omp-role='omp.prefs.editor'] input", value: "2.9" }, { type: "press", selector: "[data-omp-role='omp.prefs.editor'] input", key: "Enter" }], comparisons: [selectedLabel(providerDefinition), ...effects("providers.maxInFlightRequests", { alpha: 2 })] },
    { name: "clear all", terminalInput: "\r\x1b[B\x1b[B\r", browserActions: [{ type: "click", selector: `${settingRow(providerDefinition.path)} button` }, { type: "click", selector: "[data-omp-role='omp.select'] button:has-text('Clear all limits')" }], comparisons: [selectedLabel(providerDefinition), ...effects("providers.maxInFlightRequests", {}, 1)] },
  ],
}];
const inheritedDefinition = settingDefinitions.find(entry => entry.definition.path === "plan.autosaveDir")?.definition;
if (!inheritedDefinition) throw new Error("Canonical plan.autosaveDir editor missing");
export const provenanceFixtureCases: FixtureCase[] = [{ id: "settings-project-inheritance", covers: ["setting:plan.autosaveDir", "settings:project", "settings:inherited"],
  async create(context) {
    await mkdir(`${context.workspace}/.omp`, { recursive: true });
    await Bun.write(`${context.workspace}/.omp/settings.json`, JSON.stringify({ plan: { autosaveDir: "/project-plan" } }));
    const { Settings } = await context.load<typeof import("@omp-source/packages/coding-agent/src/config/settings")>("packages/coding-agent/src/config/settings.ts");
    await Settings.instance.reloadForCwd(context.workspace);
    await Settings.instance.reloadFromDisk();
    return createSettings(context, inheritedDefinition, { "plan.autosaveDir": "/global-plan" });
  }, checkpoints: [
    { name: "project owns displayed value", comparisons: [selectedLabel(inheritedDefinition), callbackValue("project provenance", "provenance", "project"),
      { name: "inherited directory value", terminal: { type: "screen", pattern: "(/project-plan)", group: 1 }, browser: { type: "dom", selector: `button[aria-label=${attribute(inheritedDefinition.label)}]`, read: "text" }, expected: "/project-plan" }] },
    { name: "global edit cannot replace project authority", terminalInput: "\r\x01\x0b/changed-global\r", browserActions: [
      { type: "click", selector: `button[aria-label=${attribute(inheritedDefinition.label)}]` }, { type: "fill", selector: `input[aria-label=${attribute(inheritedDefinition.label)}]`, value: "/changed-global" }, { type: "press", selector: `input[aria-label=${attribute(inheritedDefinition.label)}]`, key: "Enter" },
    ], comparisons: [selectedLabel(inheritedDefinition), ...effects("plan.autosaveDir", "/project-plan")] },
  ],
}];
export const delayedEditingFixtureCases: FixtureCase[] = [{ id: "settings-delayed-unicode-edit", covers: ["setting:plan.autosaveDir", "settings:delayed-edit", "settings:unicode"],
  create: context => createSettings(context, inheritedDefinition), checkpoints: [
    { name: "open actual text field", terminalInput: "\r", browserActions: [{ type: "click", selector: `button[aria-label=${attribute(inheritedDefinition.label)}]` }], comparisons: [selectedLabel(inheritedDefinition)] },
    { name: "delayed authoritative echo cannot overwrite newer Unicode draft", terminalInput: "\x01\x0bfirst\x01\x0bnew-π-漢字-🙂\r", browserActions: [
      { type: "transportDelay", milliseconds: 300, direction: "incoming" },
      { type: "fill", selector: `input[aria-label=${attribute(inheritedDefinition.label)}]`, value: "first" },
      { type: "fill", selector: `input[aria-label=${attribute(inheritedDefinition.label)}]`, value: "new-π-漢字-🙂" },
      { type: "press", selector: `input[aria-label=${attribute(inheritedDefinition.label)}]`, key: "Enter" },
    ], comparisons: [selectedLabel(inheritedDefinition), ...effects("plan.autosaveDir", "new-π-漢字-🙂")] },
    { name: "restore transport", browserActions: [{ type: "transportDelay", milliseconds: 0 }], comparisons: [selectedLabel(inheritedDefinition)] },
  ],
}];
export const fixtureCases: FixtureCase[] = [...conditionFixtureCases, ...tabFixtureCases, ...providerLimitsFixtureCases, ...provenanceFixtureCases, ...delayedEditingFixtureCases];
