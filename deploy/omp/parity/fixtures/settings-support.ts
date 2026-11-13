import { orderedSettings } from "@omp-source/packages/coding-agent/src/config/all-settings";
import { getSupportedEfforts } from "@omp-source/packages/catalog/src/model-thinking";
import type { Model } from "@omp-source/packages/catalog/src/types";
import { getSettingDef, type SettingDef } from "@omp-source/packages/tui/src/overlays/settings-defs";
import type { SettingsUiCapabilities } from "@omp-source/packages/coding-agent/src/config/settings-ui";
import type { PluginSettingsHost } from "@omp-source/packages/tui/src/overlays/plugin-settings";
import type { FixtureContext, FixtureCase, Comparison, BrowserAction } from "../scenario";
import { visibleText } from "../scenario";
import { probe } from "../probe";
import { readRuntimeSettingsCatalog } from "../inventory-runtime-settings";
import { sourceLoader } from "../source-load";

const sourceRoot = process.env.OMP_PARITY_SOURCE_ROOT;
if (!sourceRoot) throw new Error("Settings fixtures require an owned source root after HOME isolation");
export const runtimeSettingsCatalog = await readRuntimeSettingsCatalog(sourceLoader(sourceRoot));
export const availableThemeNames = [...runtimeSettingsCatalog.themes];
export const settingDefinitions = orderedSettings().flatMap(setting => {
  const definition = getSettingDef([{ path: setting.id, type: setting.type, defaultValue: setting.default,
    ui: setting.ui, enumValues: setting.enumValues, credential: setting.isCredential }], setting.id);
  return definition ? [{ setting, definition }] : [];
});
export const escapePattern = (value: string): string => value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
export const attribute = (value: string): string => JSON.stringify(value);
export const labelControl = (label: string): string => `[aria-label=${attribute(label)}]`;
export const rowLabel = (label: string): string => `p.font-medium:text-matches(${attribute(`^${escapePattern(label)}(?:\\s*•)?$`)})`;
export const selectedLabel = ({ path, label }: Pick<SettingDef, "path" | "label">): Comparison => {
  const selector = `[data-omp-setting=${attribute(path)}] ${rowLabel(label)}`;
  return { ...visibleText("setting row", `(${escapePattern(label)})`, selector, label),
    browser: { type: "dom", selector, read: "text", pattern: `(${escapePattern(label)})`, group: 1 },
    proves: [{ entry: "property:TspPrefsRow.label", effect: "literal", value: label }] };
};

const enabledSeeds: Record<string, Record<string, unknown>> = {
  advisorEnabled: { "advisor.enabled": true }, vimModeEnabled: { "tui.vimMode": true },
  composerPredictionsEnabled: { "composer.predictions": true }, hindsightActive: { "hindsight.apiUrl": "", "memory.backend": "hindsight" },
  mnemopiActive: { "memory.backend": "mnemopi" }, autolearnActive: { "autolearn.enabled": true },
  autoThinkingActive: { defaultThinkingLevel: "auto" }, usageAwareFallbackEnabled: { "retry.usageAwareFallback": true },
  planModeEnabled: { "plan.enabled": true }, planAutosaveEnabled: { "plan.enabled": true, "plan.autosave": true },
  contextFilesExtraNotProject: {}, restrictedFoldersNotProject: {}, perCallModelAllowNotProject: {},
  macOS: {}, hasImageProtocol: {}, nativeRendering: {},
};

export async function createSettings(context: FixtureContext, definition?: SettingDef, seed: Record<string, unknown> = {},
  options: { hidden?: boolean; capabilities?: SettingsUiCapabilities; model?: Model; plugins?: PluginSettingsHost } = {}) {
  const { Settings } = await context.load<typeof import("@omp-source/packages/coding-agent/src/config/settings")>("packages/coding-agent/src/config/settings.ts");
  const { lookup } = await context.load<typeof import("@omp-source/packages/coding-agent/src/config/registry")>("packages/coding-agent/src/config/registry.ts");
  const { createSettingsHost } = await context.load<typeof import("@omp-source/packages/coding-agent/src/config/settings-ui")>("packages/coding-agent/src/config/settings-ui.ts");
  const { createPluginSettingsHost } = await context.load<typeof import("@omp-source/packages/coding-agent/src/extensibility/plugins/settings-host")>("packages/coding-agent/src/extensibility/plugins/settings-host.ts");
  const { SettingsSelectorComponent } = await context.load<typeof import("@omp-source/packages/tui/src/overlays/settings-selector")>("packages/tui/src/overlays/settings-selector.ts");
  const { previewTheme } = await context.load<typeof import("@omp-source/packages/tui/src/theme/theme")>("packages/tui/src/theme/theme.ts");
  const status = await context.load<typeof import("@omp-source/packages/coding-agent/src/modes/settings")>("packages/coding-agent/src/modes/settings.ts");
  const live = probe.requireInteractiveContext();
  const condition = definition && settingDefinitions.find(entry => entry.definition.path === definition.path)?.setting.ui?.condition;
  if (condition && !(condition in enabledSeeds)) throw new Error(`Unseeded settings condition ${condition}`);
  const store = Settings.instance;
  const host = createSettingsHost(store, options.capabilities ?? { macOS: true, hasImageProtocol: true, nativeRendering: true });
  for (const [path, value] of Object.entries({ ...(condition ? enabledSeeds[condition] : {}), ...seed })) host.set(path, value);
  await store.flush();
  let closing = false;
  const model = options.model ?? probe.requireSession().model;
  const component = new SettingsSelectorComponent({ settings: host, plugins: options.plugins ?? createPluginSettingsHost(context.workspace),
    availableThinkingLevels: model ? [...getSupportedEfforts(model)] : [], thinkingLevel: undefined,
    availableThemes: availableThemeNames, providers: ["alpha", "beta"], requestRender: () => context.tui.requestRender(),
    composerPreviewStatus: live.statusLine, imageBudget: context.tui.imageBudget, model }, {
    onChange: (path, value) => {
      // An unset setting reads back as undefined, which the effect log would drop; null keeps the unset observable on both legs.
      context.record(`change:${path}`, value ?? null);
      void store.flush().then(async () => {
        const restored = await Settings.loadIsolated({ cwd: context.workspace });
        context.record(`saved:${path}`, lookup(path)?.layered(restored) ?? null);
      });
    },
    onThemePreview: async value => {
      context.record("theme-preview", value);
      const result = await previewTheme(value);
      if (!result.success) throw new Error(result.error ?? `Theme preview failed: ${value}`);
      live.statusLine.invalidate();
      context.tui.invalidate();
      context.tui.requestRender();
    },
    onStatusLinePreview: value => {
      context.record("status-preview", value);
      live.statusLine.updateSettings({
        preset: status.cfgStatusLinePreset.get(store), leftSegments: status.cfgStatusLineLeftSegments.get(store),
        rightSegments: status.cfgStatusLineRightSegments.get(store), separator: status.cfgStatusLineSeparator.get(store),
        showHookStatus: status.cfgStatusLineShowHookStatus.get(store), sessionAccent: status.cfgStatusLineSessionAccent.get(store),
        transparent: status.cfgStatusLineTransparent.get(store), compactThinkingLevel: status.cfgStatusLineCompactThinkingLevel.get(store),
        contextLine: status.cfgStatusLineContextLine.get(store), segmentOptions: status.cfgStatusLineSegmentOptions.get(store), ...value,
      });
      context.tui.requestRender();
    },
    getStatusLinePreview: () => live.statusLine.getPreviewLines(live.editor.getTopBorderAvailableWidth(context.tui.terminal.columns)).join("\n"),
    describeStatusLinePreview: () => live.statusLine.describePreview(),
    onCancel: () => {
      if (closing) return;
      closing = true;
      void store.flush().then(async () => {
        const restored = await Settings.loadIsolated({ cwd: context.workspace });
        if (definition) context.record(`closed-value:${definition.path}`, lookup(definition.path)?.layered(restored));
        context.record("closed", true);
        context.done();
      });
    },
  });
  if (definition) {
    const actual = host.entries.find(entry => entry.path === definition.path);
    if (!actual) throw new Error(`Missing canonical setting ${definition.path}`);
    const visible = actual.condition ? actual.condition() : true;
    if (visible === Boolean(options.hidden)) throw new Error(`Unexpected visibility for ${definition.path} (${condition})`);
    context.record("initial-value", host.get(definition.path));
    context.record("provenance", lookup(definition.path)?.provenance(store));
    for (const character of definition.path) component.handleInput(character);
    if (!options.hidden) focusSearchResult(component, definition.path);
    context.record("visibility", condition ? probe.conditionOutcome(definition.path) : visible);
  }
  return component;
}

interface SearchableSettings {
  describe(context: { cols: number; dark: boolean; reduceMotion: boolean; supports(kind: string): boolean; feature(): boolean }): { k: string; p?: unknown };
  handleInput(data: string): void;
}

/**
 * OMP's settings search keeps the previous selection by row id across
 * keystrokes, so the typed path can leave a sibling match selected. Moving the
 * program's own selection with Down makes both legs start on the target row.
 */
function focusSearchResult(component: SearchableSettings, path: string): void {
  const read = () => {
    const described = component.describe({ cols: 120, dark: true, reduceMotion: true, supports: kind => kind === "prefs", feature: () => false });
    const props = described.p as { focus?: string | null; sections?: { rows: unknown[] }[] } | undefined;
    if (described.k !== "prefs" || !props?.sections) throw new Error(`Settings search for ${path} did not describe prefs`);
    return { focus: props.focus, rows: props.sections.reduce((count, section) => count + section.rows.length, 0) };
  };
  const rows = read().rows;
  for (let step = 0; read().focus !== path; step++) {
    if (step > rows) throw new Error(`Settings search cannot focus ${path}`);
    component.handleInput("\x1b[B");
  }
}

export function effects(path: string, expected: unknown, occurrence = 0): Comparison[] {
  return [
    { name: "canonical setting callback", terminal: { type: "effect", channel: `change:${path}`, occurrence }, browser: { type: "effect", channel: `change:${path}`, occurrence }, expected: expected ?? null },
    { name: "persisted setting reload", terminal: { type: "effect", channel: `saved:${path}`, occurrence }, browser: { type: "effect", channel: `saved:${path}`, occurrence }, expected: expected ?? null },
  ];
}
export function textActions(definition: SettingDef, value: string): BrowserAction[] {
  const selector = `input${labelControl(definition.label)}`;
  const edit: BrowserAction = value || definition.type !== "text" || !definition.secret
    ? { type: "fill", selector, value }
    : { type: "press", selector, key: "Backspace" };
  return [{ type: "click", selector: `:is(button,input[type=password])${labelControl(definition.label)}` }, edit, { type: "press", selector, key: "Enter" }];
}
export function caseId(path: string): string { return path.replace(/[^a-zA-Z0-9]+/g, "-"); }
export type SettingsCaseFactory = (definition: SettingDef) => FixtureCase[];
