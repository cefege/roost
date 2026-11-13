// Search-mode settings pages built by the real SettingsSelectorComponent: one case per control kind,
// each proving the typed TspPrefs* properties its row carries against the rendered TUI and DOM.
import type { SettingDef } from "@omp-source/packages/tui/src/overlays/settings-defs";
import type { FixtureCase } from "../scenario";
import { createSettings, escapePattern, selectedLabel, settingDefinitions } from "./settings-support";
import { ariaLabel, changedDotSelector, claim, controlBesideLabel, hintSelector, labelAtRowStart, literal, literalFieldValue, prefsCase, rowSelector, settingsComponent, warningSelector } from "./prefs-support";

const unambiguousEscape = "\x1b[27u";

function definitionOf(path: string): SettingDef {
  const found = settingDefinitions.find(entry => entry.definition.path === path)?.definition;
  if (!found) throw new Error(`Missing canonical setting ${path}`);
  return found;
}
const withoutUndefined = <Value extends object>(value: Value): Value => JSON.parse(JSON.stringify(value)) as Value;
const describedOptions = (definition: SettingDef) => definition.type === "submenu" || definition.type === "multiselect"
  ? definition.options.map(option => withoutUndefined({ value: option.value, label: option.label, detail: option.description })) : [];

function switchCase(id: string, path: string, seed: Record<string, unknown>, on: boolean, extras: { changedFrom?: string } = {}): FixtureCase {
  const definition = definitionOf(path);
  const row = rowSelector(path);
  const warning = definition.warning;
  const description = definition.description ?? "";
  return prefsCase(id, settingsComponent, context => createSettings(context, definition, seed), [{
    name: "switch row", comparisons: [
      selectedLabel(definition),
      literal("row hint", escapePattern(description), hintSelector(path), description, [claim("TspPrefsRow.hint", "literal", description)]),
      ...(warning ? [literal("row warning", escapePattern(warning), warningSelector(path), warning, [claim("TspPrefsRow.warning", "literal", warning)])] : []),
      controlBesideLabel("switch beside label", definition.label, String(on), `${row} [role=switch]`, [
        claim("TspPrefsControl.k", "layout", "switch"), claim("TspPrefsControl.on", "layout", on),
        claim("TspPrefsRow.control", "layout", { k: "switch", on }), claim("TspPrefsRow.id", "layout", path)]),
      ...(extras.changedFrom !== undefined ? [labelAtRowStart("changed marker beside label", definition.label, changedDotSelector(path), [
        claim("TspPrefsRow.changed", "layout", true), claim("TspPrefsRow.defaultLabel", "layout", extras.changedFrom)])] : []),
    ],
  }]);
}

function numberCase(id: string, path: string, current: number, shown: string, defaultShown: string): FixtureCase {
  const definition = definitionOf(path);
  const row = rowSelector(path);
  const description = definition.description ?? "";
  const labels = Object.fromEntries(definition.type === "submenu" ? definition.options.map(option => [String(Number(option.value)), option.label]) : []);
  return prefsCase(id, settingsComponent, context => createSettings(context, definition, { [path]: current }), [{
    name: "number row", comparisons: [
      selectedLabel(definition),
      literal("row hint", escapePattern(description), hintSelector(path), description, [claim("TspPrefsRow.hint", "literal", description)]),
      controlBesideLabel("number beside label", definition.label, `(?<![\\d.])${escapePattern(current.toString())}`, `${row} input[type=number]`, [
        claim("TspPrefsControl.k", "layout", "number"), claim("TspPrefsControl.value", "layout", current),
        claim("TspPrefsControl.labels", "layout", labels), claim("TspPrefsRow.control", "layout", { k: "number", value: current, labels })]),
      labelAtRowStart("changed marker beside label", definition.label, changedDotSelector(path), [
        claim("TspPrefsRow.changed", "layout", true), claim("TspPrefsRow.defaultLabel", "layout", defaultShown)]),
    ],
  }, {
    name: "open step list", terminalInput: "\r", browserActions: [{ type: "click", selector: `${row} input[type=number]` }], comparisons: [
      literal("step label", `(?<![\\d.])${escapePattern(shown)}`, `${row} [data-omp-step-label]`, shown, [claim("TspPrefsControl.labels[key]", "literal", shown)]),
    ],
  }]);
}

function menuCase(id: string, path: string, current: string, defaultValue: string, style: "menu" | "auto", mono: boolean): FixtureCase {
  const definition = definitionOf(path);
  const row = rowSelector(path);
  const options = describedOptions(definition);
  const chosen = options.find(option => option.value === current);
  const optionRow = (value: string) => `${row} [data-name=CommandItem][data-value=${JSON.stringify(value)}]`;
  return prefsCase(id, settingsComponent, context => createSettings(context, definition, { [path]: current }), [
    { name: "choice row", comparisons: [
      selectedLabel(definition),
      controlBesideLabel("choice beside label", definition.label, escapePattern(current), `${row} button${ariaLabel(definition.label)}`, [
        claim("TspPrefsControl.k", "layout", "choice"), claim("TspPrefsControl.style", "layout", style), claim("TspPrefsControl.mono", "layout", mono),
        claim("TspPrefsRow.control", "layout", withoutUndefined({ k: "choice", value: current, options, style, mono }))]),
      labelAtRowStart("changed marker beside label", definition.label, changedDotSelector(path), [
        claim("TspPrefsRow.changed", "layout", true), claim("TspPrefsRow.defaultLabel", "layout", options.find(option => option.value === defaultValue)?.label ?? defaultValue)]),
    ] },
    { name: "open option menu", terminalInput: "\r", browserActions: [{ type: "click", selector: `${row} button${ariaLabel(definition.label)}` }], comparisons: [
      ...options.map(option => literal(`option ${option.value}`, escapePattern(option.label), `${optionRow(option.value)} span:nth-child(1)`, option.label,
        [claim("TspPrefsControl.options[].label", "literal", option.label), ...(option.label === option.value ? [claim("TspPrefsControl.options[].value", "literal", option.value)] : [])])),
      ...options.filter(option => option.detail !== undefined).map(option => literal(`detail ${option.value}`, escapePattern(option.detail ?? ""), `${optionRow(option.value)} span:nth-child(2)`, option.detail ?? "",
        [claim("TspPrefsControl.options[].detail", "literal", option.detail)])),
      literal("highlighted option", `(?<=❯ )${escapePattern(chosen?.label ?? current)}`, `${row} [data-name=CommandItem][aria-selected=true] span:nth-child(1)`, chosen?.label ?? current,
        [claim("TspPrefsProps.editing.option", "literal", current)]),
    ] },
    { name: "dismiss option menu", terminalInput: unambiguousEscape, browserActions: [{ type: "press", key: "Escape" }], comparisons: [
      controlBesideLabel("value unchanged after dismiss", definition.label, escapePattern(current), `${row} button${ariaLabel(definition.label)}`, [claim("TspPrefsProps.editing", "layout", null)])] },
  ]);
}

function textCase(id: string, path: string, seed: unknown, shown: string, secret: boolean): FixtureCase {
  const definition = definitionOf(path);
  const row = rowSelector(path);
  // A set secret renders as a read-only password field so the browser shows a masked control, not bullet text.
  const control = secret ? `${row} input[type=password]${ariaLabel(definition.label)}` : `${row} button${ariaLabel(definition.label)}`;
  const claims = [claim("TspPrefsControl.k", "layout", "text"), claim(secret ? "TspPrefsControl.secret" : "TspPrefsControl.mono", "layout", true)];
  return prefsCase(id, settingsComponent, context => createSettings(context, definition, { [path]: seed }), [{
    name: "text row", comparisons: [
      selectedLabel(definition),
      (secret ? literalFieldValue : literal)("text value", `(?<=${escapePattern(definition.label)}[^\\n]*?)${escapePattern(shown)}`, control, shown, [claim("TspPrefsControl.value", "literal", shown)]),
      controlBesideLabel("text control beside label", definition.label, escapePattern(shown), control, claims),
    ],
  }]);
}

const page = "[data-omp-page]";
const secretMask = "••••••••";

function editTextCase(id: string, path: string, seed: string, draft: string, secret: boolean): FixtureCase {
  const definition = definitionOf(path);
  const row = rowSelector(path);
  const field = `${row} input:not([type="file"])`;
  const draftPattern = secret ? `(?<=> )•{${draft.length}}` : `(?<=> )${escapePattern(draft)}`;
  return prefsCase(id, settingsComponent, context => createSettings(context, definition, { [path]: seed }), [
    { name: "text row", comparisons: [selectedLabel(definition)] },
    { name: "edit text in place", terminalInput: "\r", browserActions: [{ type: "click", selector: secret ? `${row} input[type=password]${ariaLabel(definition.label)}` : `${row} button${ariaLabel(definition.label)}` }], comparisons: [
      // The terminal prints the draft on its own line below the label; the browser keeps its input in the row, so only the shared top-of-screen placement is comparable.
      ...(secret ? [] : [literalFieldValue("draft", draftPattern, field, draft, [claim("TspPrefsProps.editing.draft", "literal", draft)])]),
      { name: "draft sits in the upper half", terminal: { type: "screen", pattern: draftPattern, read: "topHalf" }, browser: { type: "dom", selector: field, read: "topHalf", relativeTo: page }, expected: true,
        proves: [claim("TspPrefsProps.editing.row", "layout", path), claim("TspPrefsProps.editing.cursor", "layout", seed.length), ...(secret ? [] : [claim("TspPrefsProps.editing.secret", "layout", false)])] },
      ...(secret ? [{ name: "secret draft stays masked", terminal: { type: "screen" as const, pattern: draftPattern, read: "visible" as const }, browser: { type: "dom" as const, selector: `${field}[type=password]`, read: "mask" as const }, expected: true, proves: [claim("TspPrefsProps.editing.secret", "layout", true)] }] : []),
    ] },
  ]);
}

function multiCase(): FixtureCase {
  const definition = definitionOf("compaction.methodOrder");
  const path = definition.path;
  const row = rowSelector(path);
  const options = describedOptions(definition);
  const values = definition.type === "multiselect" ? definition.defaultValue as string[] : [];
  return prefsCase("prefs-search-multi-ordered", settingsComponent, context => createSettings(context, definition), [{
    name: "ordered multi row", comparisons: [
      selectedLabel(definition),
      controlBesideLabel("ordered multi beside label", definition.label, escapePattern(options[0]?.label ?? ""), `${row} button[aria-label=${JSON.stringify(`Move ${values[0]} down`)}]`, [
        claim("TspPrefsControl.k", "layout", "multi"), claim("TspPrefsControl.values", "layout", values), claim("TspPrefsControl.options", "layout", options),
        claim("TspPrefsControl.ordered", "layout", true)]),
    ],
  }]);
}

function actionCase(): FixtureCase {
  const definition = definitionOf("providers.maxInFlightRequests");
  const path = definition.path;
  const row = rowSelector(path);
  return prefsCase("prefs-search-action-limits", settingsComponent, context => createSettings(context, definition, { [path]: { alpha: 2 } }), [{
    name: "action row", comparisons: [
      selectedLabel(definition),
      literal("action label", `(?<=${escapePattern(definition.label)}[^\\n]*?)alpha: 2`, `${row} button:text-is("alpha: 2")`, "alpha: 2", [claim("TspPrefsControl.label", "literal", "alpha: 2")]),
      controlBesideLabel("action beside label", definition.label, "alpha: 2", `${row} button:text-is("alpha: 2")`, [
        claim("TspPrefsControl.k", "layout", "action"), claim("TspPrefsControl.act", "layout", "edit")]),
    ],
  }]);
}

export const fixtureCases: FixtureCase[] = [
  switchCase("prefs-search-switch-warning-on", "externalThinking", { externalThinking: true }, true, { changedFrom: "Off" }),
  switchCase("prefs-search-switch-off", "colorBlindMode", {}, false),
  numberCase("prefs-search-number-steps", "autocompleteMaxVisible", 7, "7 items", "10 items"),
  menuCase("prefs-search-menu-detail", "ttsr.interruptMode", "prose-only", "always", "menu", true),
  menuCase("prefs-search-menu-auto", "update.channel", "canary", "stable", "auto", false),
  textCase("prefs-search-text-mono", "sharpshooter.model", "parity-model", "parity-model", false),
  textCase("prefs-search-text-secret", "mnemopi.embeddingApiKey", "parity-secret", secretMask, true),
  editTextCase("prefs-edit-text-mono", "sharpshooter.model", "parity-model", "parity-model", false),
  editTextCase("prefs-edit-text-secret", "mnemopi.embeddingApiKey", "parity-secret", "•".repeat("parity-secret".length), true),
  multiCase(),
  actionCase(),
];
