import type { FixtureCase, BrowserAction } from "../scenario";
import { visibleText, callbackValue } from "../scenario";
import { getComposerShapeOptions } from "@omp-source/packages/tui/src/overlays/composer-shape-registry";
import { numberSteps, type SettingDef } from "@omp-source/packages/tui/src/overlays/settings-defs";
import { attribute, availableThemeNames, caseId, createSettings, effects, escapePattern, labelControl, selectedLabel, settingDefinitions, textActions, runtimeSettingsCatalog } from "./settings-support";
import { conditionFixtureCases, tabFixtureCases, providerLimitsFixtureCases, provenanceFixtureCases, delayedEditingFixtureCases } from "./settings-transactions";
import { fixtureCases as shapePreviewFixtureCases } from "./settings-previews";
import { proveSettingsFixture, renderedSettingValue, settingRow } from "./settings-proofs";
import { optionEnumeration } from "./settings-options";
import { prefsFixtureCases } from "./prefs-cases";
import type { RuntimeThinkingVariant } from "../inventory-runtime-settings";
export { composerPreviewFixtureCases, snapcompactPreviewFixtureCases } from "./settings-previews";

const down = "\x1b[B";
const clear = "\x01\x0b";
const previewed = new Set(["theme.dark", "theme.light", "statusLine.preset", "statusLine.separator", "statusLine.contextLine", "snapcompact.shape", "composer.shape"]);
const largePickerThreshold = 12;

function choices(definition: SettingDef, variant?: RuntimeThinkingVariant): readonly { value: string; label: string; description?: string }[] {
  if (definition.type === "enum") return definition.values.map(value => ({ value, label: value }));
  if (definition.type !== "submenu" && definition.type !== "multiselect") return [];
  if (definition.path === "theme.dark" || definition.path === "theme.light") return availableThemeNames.map(value => ({ value, label: value }));
  if (definition.path === "composer.shape") return getComposerShapeOptions();
  if (definition.path === "defaultThinkingLevel") {
    if (!variant) throw new Error("Thinking settings require a real bundled model capability context");
    return ["auto", ...variant.efforts].map(value => definition.options.find(option => option.value === value) ?? { value, label: value });
  }
  return definition.options;
}
function isPicker(definition: SettingDef, options: readonly unknown[]): boolean {
  return definition.path === "theme.dark" || definition.path === "theme.light" || options.length > largePickerThreshold;
}
function choiceActions(definition: SettingDef, choice: { value: string; label: string }, options: readonly unknown[], opened = false): BrowserAction[] {
  const control = labelControl(definition.label);
  if (numberSteps(definition)) {
    return [{ type: "fill", selector: `input${control}`, value: choice.value === "default" ? "-1" : choice.value },
      { type: "press", selector: `input${control}`, key: "Tab" }];
  }
  const pickerConfirm: BrowserAction = { type: "click", selector: `button:has(kbd:text-is("enter"))` };
  const pickerItem: BrowserAction = { type: "click", selector: `[data-omp-picker-item]:has(:text-is(${attribute(choice.label)}))` };
  const open: BrowserAction[] = opened ? [] : [{ type: "click", selector: `button${control}` }];
  if (isPicker(definition, options)) return [...open, pickerItem, pickerConfirm];
  return [...open, { type: "click", selector: `[role=option]:has(span:text-is(${attribute(choice.label)}))` }];
}
function expectedChoice(definition: SettingDef, value: string): unknown {
  return definition.schemaType === "number" ? value === "default" ? -1 : Number(value) : value;
}
function optionCases(definition: SettingDef, variant?: RuntimeThinkingVariant): FixtureCase[] {
  const options = choices(definition, variant);
  const runtime = ["theme.dark", "theme.light", "composer.shape", "defaultThinkingLevel"].includes(definition.path);
  const state = variant?.id ?? "registered";
  const runtimeEntries = runtime ? [`setting-options-factory:${definition.path}`, `setting-options-state:${definition.path}=${state}`] : [];
  if (options.length === 0) throw new Error(`No real choices for ${definition.path}`);
  return options.map((choice, index) => {
    const initial = options[(index + options.length - 1) % options.length];
    if (!initial) throw new Error(`Missing initial choice for ${definition.path}`);
    // One case per runtime state opens the list and reads every option on both legs; the rest only choose.
    const enumeration = runtime && index === 0 ? optionEnumeration(definition, options.map(option => option.label), isPicker(definition, options)) : undefined;
    const caseEntries = enumeration ? runtimeEntries : [];
    return {
      id: `settings-${caseId(definition.path)}-choice-${index}${variant ? `-state-${caseId(variant.id)}` : ""}`,
      covers: [`setting:${definition.path}`, `setting-choice:${definition.path}=${choice.value}`, `control:${definition.type}`, ...caseEntries, ...(settingDefinitions.find(entry => entry.definition.path === definition.path)?.setting.ui?.condition ? [`setting-condition:${definition.path}:true`] : [])],
      create: context => createSettings(context, definition, { [definition.path]: expectedChoice(definition, initial.value) }, { model: variant?.model }),
      checkpoints: [{ name: "canonical row", comparisons: [selectedLabel(definition), ...renderedValueComparisons(definition, expectedChoice(definition, initial.value))] }, ...(enumeration?.checkpoints ?? []), {
        name: "choose and persist", terminalInput: definition.type === "enum" ? "\r" : `${enumeration ? "" : "\r"}\x1b[H${down.repeat(index)}\r`,
        browserActions: choiceActions(definition, choice, options, enumeration !== undefined),
        comparisons: [selectedLabel(definition), ...effects(definition.path, expectedChoice(definition, choice.value)).map(comparison => comparison.name === "canonical setting callback" && definition.schemaType === "number" ? { ...comparison, expected: choice.value } : comparison.name !== "persisted setting reload" ? comparison : {
          ...comparison, proves: caseEntries.map(entry => ({ entry, effect: "interaction" as const, value: options.map(option => option.value),
            target: { checkpoint: "choose and persist", comparison: "rendered setting value" }, assertionGroups: [{ comparisons: [
              ...(enumeration?.observations ?? []),
              { checkpoint: "choose and persist", comparison: "rendered setting value" }, { checkpoint: "choose and persist", comparison: "canonical setting callback" },
              { checkpoint: "choose and persist", comparison: "persisted setting reload" },
            ] }] })),
        })],
      }],
    };
  });
}
function booleanCases(definition: SettingDef): FixtureCase[] {
  return [false, true].map(initial => ({ id: `settings-${caseId(definition.path)}-${initial}`,
    covers: [`setting:${definition.path}`, "control:boolean"],
    create: context => createSettings(context, definition, { [definition.path]: initial }),
    checkpoints: [{ name: "initial row", comparisons: [selectedLabel(definition)] }, {
      name: "toggle", terminalInput: "\r", browserActions: [{ type: "click", selector: `[role=switch]${labelControl(definition.label)}` }],
      comparisons: [selectedLabel(definition), ...effects(definition.path, !initial)],
    }],
  }));
}
function textValue(definition: SettingDef): string {
  if (definition.schemaType === "array") {
    if (definition.path === "contextFiles.extra") return '["TEAM-π.md","規約.md"]';
    if (definition.path === "task.perCallModelAllow") return '["alpha/model"]';
    return '["parity-π"]';
  }
  if (definition.schemaType === "record") {
    if (definition.path === "tools.approval") return '{"bash":"prompt","write":"deny"}';
    if (definition.path === "retry.fallbackChains") return '{"default":["openai/parity-small:high"]}';
    throw new Error(`Missing canonical record-editor seed for ${definition.path}`);
  }
  return definition.path.toLowerCase().endsWith("url") || definition.path.toLowerCase().endsWith("endpoint") ? "http://127.0.0.1:1/parity-π" : "parity-π-漢字-🙂";
}
function textCases(definition: SettingDef): FixtureCase[] {
  const value = textValue(definition);
  const expected: unknown = definition.schemaType === "array" || definition.schemaType === "record" ? JSON.parse(value) : value;
  return [{ id: `settings-${caseId(definition.path)}-text`, covers: [`setting:${definition.path}`, "control:text", ...(definition.schemaType === "array" || definition.schemaType === "record" ? ["control:editor"] : []), ...(definition.type === "text" && definition.secret ? ["settings:secret"] : [])],
    create: context => createSettings(context, definition), checkpoints: [
      { name: "canonical row", comparisons: [selectedLabel(definition)] },
      { name: "cancel distinct draft", terminalInput: `\r${clear}cancel-only-π\x1b`, browserActions: [
        { type: "click", selector: `:is(button,input[type=password])${labelControl(definition.label)}` },
        { type: "fill", selector: `input${labelControl(definition.label)}`, value: "cancel-only-π" },
        { type: "press", selector: `input${labelControl(definition.label)}`, key: "Escape" }], comparisons: [selectedLabel(definition)] },
      { name: "edit Unicode draft", terminalInput: `\r${clear}${value}`, browserActions: textActions(definition, value).slice(0, -1), comparisons: [definition.type === "text" && definition.secret
        ? { name: "secret remains masked", terminal: { type: "screen", pattern: `(?<!•)•{${Array.from(value).length}}(?!•)`, read: "visible" }, browser: { type: "dom", selector: `input[type=password]${labelControl(definition.label)}`, read: "mask" }, expected: true }
        : { name: "Unicode draft contents", terminal: { type: "screen", pattern: `(${escapePattern(value)})`, group: 1 }, browser: { type: "dom", selector: `input${labelControl(definition.label)}`, read: "value" }, expected: value }] },
      { name: "commit Unicode draft", terminalInput: "\r", browserActions: [{ type: "press", selector: `input${labelControl(definition.label)}`, key: "Enter" }], comparisons: [selectedLabel(definition), ...effects(definition.path, expected)] },
    ],
  }, { id: `settings-${caseId(definition.path)}-unset`, covers: [`setting:${definition.path}`, "control:text", "settings:unset"],
    create: context => createSettings(context, definition, { [definition.path]: expected }), checkpoints: [
      { name: "configured row", comparisons: [selectedLabel(definition)] },
      { name: "clear configured value", terminalInput: `\r${clear}\r`, browserActions: textActions(definition, ""), comparisons: [selectedLabel(definition), ...effects(definition.path, definition.defaultValue)] },
    ],
  }, { id: `settings-${caseId(definition.path)}-cancel-persistence`, covers: [`setting:${definition.path}`, "control:text"], behaviors: ["settings:cancel"],
    create: context => createSettings(context, definition, { [definition.path]: expected }), checkpoints: [
      { name: "configured value before cancelled draft", comparisons: [selectedLabel(definition), ...renderedValueComparisons(definition, expected)] },
      { name: "cancel draft restores displayed value", terminalInput: `\r${clear}cancel-only-π\x1b`, browserActions: [
        { type: "click", selector: `:is(button,input[type=password])${labelControl(definition.label)}` },
        { type: "fill", selector: `input${labelControl(definition.label)}`, value: "cancel-only-π" },
        { type: "press", selector: `input${labelControl(definition.label)}`, key: "Escape" },
      ], comparisons: [selectedLabel(definition), ...renderedValueComparisons(definition, expected)] },
      { name: "cancelled draft survives actual settings reload", terminalInput: "\x1b\x1b", browserActions: [{ type: "press", key: "Escape" }, { type: "press", key: "Escape" }],
        comparisons: [callbackValue("cancelled draft never persisted", `closed-value:${definition.path}`, expected)] },
    ],
  }];
}
function renderedValueComparisons(definition: SettingDef, value: unknown) {
  const comparison = renderedSettingValue(definition, value);
  return comparison ? [comparison] : [];
}
function multiCases(definition: SettingDef): FixtureCase[] {
  if (definition.type !== "multiselect") return [];
  return definition.options.flatMap((option, index) => [false, true].map(initial => ({ id: `settings-${caseId(definition.path)}-member-${index}-${initial}`,
    covers: [`setting:${definition.path}`, `setting-choice:${definition.path}=${option.value}`, "control:multiselect", "component:packages/tui/src/overlays/settings-selector.ts#MultiSelectSubmenu"],
    create: context => createSettings(context, definition, { [definition.path]: initial ? [option.value] : [] }),
    checkpoints: [{ name: "canonical row", comparisons: [selectedLabel(definition)] }, {
      name: "toggle membership", terminalInput: `\r${down.repeat(index)}\r`, browserActions: [{ type: "click", selector: `[role=checkbox]${labelControl(option.label)}` }],
      comparisons: [visibleText("membership option", `(${escapePattern(option.label)})`, `${settingRow(definition.path)} label:has([aria-label=${attribute(option.label)}]) span span:first-child`, option.label),
        { name: "membership mark", terminal: { type: "screen", pattern: `${definition.ordered ? initial ? "·" : "1\\." : initial ? "○" : "●"}\\s+${escapePattern(option.label)}`, read: "visible" },
          browser: { type: "dom", selector: `${settingRow(definition.path)} [role=checkbox]${labelControl(option.label)}[aria-checked=${attribute(String(!initial))}]`, read: "visible" }, expected: true },
        ...effects(definition.path, initial ? [] : [option.value])],
    }],
  })));
}

export const booleanFixtureCases = settingDefinitions.flatMap(({ definition }) => definition.type === "boolean" ? booleanCases(definition) : []);
export const enumFixtureCases = settingDefinitions.flatMap(({ definition }) => definition.type === "enum" ? optionCases(definition) : []);
export const submenuFixtureCases = settingDefinitions.flatMap(({ definition }) => definition.type !== "submenu" ? []
  : definition.path === "defaultThinkingLevel" ? runtimeSettingsCatalog.thinking.flatMap(variant => optionCases(definition, variant)) : optionCases(definition));
export const textFixtureCases = settingDefinitions.flatMap(({ definition }) => definition.type === "text" ? textCases(definition) : []);
export const multiselectFixtureCases = settingDefinitions.flatMap(({ definition }) => multiCases(definition));
export const orderedFixtureCases: FixtureCase[] = settingDefinitions.flatMap(({ definition }) => {
  if (definition.type !== "multiselect" || !definition.ordered) return [];
  const [first, second] = definition.options;
  if (!first || !second) throw new Error(`Ordered setting lacks two choices: ${definition.path}`);
  return [{ id: `settings-${caseId(definition.path)}-reorder`, covers: [`setting:${definition.path}`, "settings:ordered"],
    create: context => createSettings(context, definition, { [definition.path]: [first.value, second.value] }), checkpoints: [
      { name: "ordered row", comparisons: [selectedLabel(definition)] },
      { name: "move selected member", terminalInput: "\r\x1b[C", browserActions: [{ type: "click", selector: `${settingRow(definition.path)} button${labelControl(`Move ${first.value} down`)}` }], comparisons: [
        selectedLabel(definition),
        visibleText("ordered first member", `1\\.\\s+(${escapePattern(second.label)})`, `${settingRow(definition.path)} div.flex.flex-wrap.gap-1 > div:first-child > span`, second.label),
        visibleText("ordered second member", `2\\.\\s+(${escapePattern(first.label)})`, `${settingRow(definition.path)} div.flex.flex-wrap.gap-1 > div:nth-child(2) > span`, first.label),
        ...effects(definition.path, [second.value, first.value]),
      ] },
    ],
  }];
});
export const previewFixtureCases: FixtureCase[] = settingDefinitions.flatMap(({ definition }) => {
  if (definition.type !== "submenu" || !previewed.has(definition.path)) return [];
  const [original, target] = choices(definition);
  if (!original || !target) throw new Error(`Preview setting lacks two choices: ${definition.path}`);
  return [{ id: `settings-${caseId(definition.path)}-preview-cancel`, covers: [`setting:${definition.path}`, "settings:preview", "settings:cancel"],
    create: context => createSettings(context, definition, { [definition.path]: expectedChoice(definition, original.value) }), checkpoints: [
      { name: "preview another choice", terminalInput: "\r\x1b[B", browserActions: [{ type: "click", selector: `button${labelControl(definition.label)}` }, { type: "hover", selector: definition.path.startsWith("theme.") ? `[data-omp-picker-item]:has(:text-is(${attribute(target.label)}))` : `[role=option]:has-text(${attribute(target.label)})` }], comparisons: [visibleText("preview choice", `(${escapePattern(target.label)})`, definition.path.startsWith("theme.") ? `[data-omp-picker-item]:has(:text-is(${attribute(target.label)})) :text-is(${attribute(target.label)})` : `[role=option]:has-text(${attribute(target.label)}) span:first-child`, target.label)] },
      { name: "cancel preview", terminalInput: "\x1b", browserActions: [{ type: "press", key: "Escape" }], comparisons: [selectedLabel(definition)] },
      { name: "close after cancelling preview", terminalInput: "\x1b\x1b", browserActions: [{ type: "press", key: "Escape" }, { type: "press", key: "Escape" }], comparisons: [callbackValue("cancelled preview is not persisted", `closed-value:${definition.path}`, expectedChoice(definition, original.value))] },
    ],
  }];
});
export const jsonEditorErrorFixtureCases: FixtureCase[] = settingDefinitions.flatMap(({ definition }) => {
  if (definition.type !== "text" || (definition.schemaType !== "array" && definition.schemaType !== "record")) return [];
  const valid = textValue(definition);
  const message = `Invalid ${definition.schemaType} JSON for ${definition.path}`;
  return ["{", definition.schemaType === "array" ? "{}" : "[]"].map((invalid, index) => ({
    id: `settings-${caseId(definition.path)}-json-error-${index}`,
    covers: [`setting:${definition.path}`, "control:editor", "settings:validation-error"],
    create: context => createSettings(context, definition), checkpoints: [
      { name: "real JSON editor", comparisons: [selectedLabel(definition)] },
      { name: "reject malformed or wrong-kind JSON", terminalInput: `\r${clear}${invalid}\r`, browserActions: textActions(definition, invalid),
        comparisons: [visibleText("canonical validation error", `(${escapePattern(message)})`, `:text-is(${attribute(message)})`, message)] },
      { name: "correct JSON and persist", terminalInput: `${clear}${valid}\r`, browserActions: [
        { type: "fill", selector: `input${labelControl(definition.label)}`, value: valid },
        { type: "press", selector: `input${labelControl(definition.label)}`, key: "Enter" },
      ], comparisons: [selectedLabel(definition), ...effects(definition.path, JSON.parse(valid))] },
    ],
  }));
});
export const editorFixtureCases = [...textFixtureCases.filter(fixture => fixture.covers.includes("control:editor")), ...jsonEditorErrorFixtureCases];
export { providerLimitsFixtureCases, tabFixtureCases, conditionFixtureCases, provenanceFixtureCases, delayedEditingFixtureCases };
export const fixtureCases: FixtureCase[] = [...booleanFixtureCases, ...enumFixtureCases, ...submenuFixtureCases, ...textFixtureCases, ...multiselectFixtureCases, ...orderedFixtureCases, ...previewFixtureCases, ...conditionFixtureCases, ...tabFixtureCases, ...providerLimitsFixtureCases, ...provenanceFixtureCases, ...delayedEditingFixtureCases, ...shapePreviewFixtureCases, ...jsonEditorErrorFixtureCases].map(fixture => {
  const path = fixture.covers.find(cover => cover.startsWith("setting:"))?.slice("setting:".length);
  const condition = settingDefinitions.find(entry => entry.definition.path === path)?.setting.ui?.condition;
  return proveSettingsFixture({ ...fixture, covers: [...new Set([...fixture.covers, "component:packages/tui/src/overlays/settings-selector.ts#SettingsSelectorComponent", ...(condition ? [`setting-condition:${condition}:true`, `setting-condition:${path}:true`] : [])])] });
}).concat(prefsFixtureCases);
