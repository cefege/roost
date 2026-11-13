import { getComposerShapeOptions } from "@omp-source/packages/tui/src/overlays/composer-shape-registry";
import { numberSteps, type SettingDef } from "@omp-source/packages/tui/src/overlays/settings-defs";
import type { BehaviorClaim, Comparison, FixtureCase } from "../scenario";
import { attribute, escapePattern, settingDefinitions, runtimeSettingsCatalog } from "./settings-support";

export const settingRow = (path: string): string => `[data-omp-setting=${attribute(path)}]`;
const runtimeValues = new Map<string, ReadonlySet<string>>([
  ["theme.dark", new Set(runtimeSettingsCatalog.themes)], ["theme.light", new Set(runtimeSettingsCatalog.themes)],
  ["composer.shape", new Set(runtimeSettingsCatalog.composers.map(option => option.value))],
  ["defaultThinkingLevel", new Set(["auto", ...runtimeSettingsCatalog.thinking.flatMap(variant => variant.efforts)])],
]);

export function nativeSettingValue(definition: SettingDef, value: unknown): unknown {
  if (definition.type === "boolean") return value === true;
  if (definition.type === "multiselect") return value;
  if (definition.type === "providerLimits") {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`Invalid expected provider map for ${definition.path}`);
    const entries = Object.entries(value).sort(([left], [right]) => left.localeCompare(right));
    return entries.length ? entries.map(([provider, limit]) => `${provider}: ${limit}`).join(", ") : "Unlimited";
  }
  if (numberSteps(definition)) return Number(value);
  if (definition.type === "text") {
    if (definition.secret) return value ? "••••••••" : "";
    return value && typeof value === "object" ? JSON.stringify(value) : String(value ?? "");
  }
  return String(value ?? "");
}

export function renderedSettingValue(definition: SettingDef, value: unknown): Comparison | undefined {
  if (definition.type === "multiselect") return undefined;
  const native = nativeSettingValue(definition, value);
  const shown = numberSteps(definition) && native === -1 ? "default" : String(native);
  if (!shown) return undefined;
  let selector: string;
  const row = settingRow(definition.path);
  if (definition.type === "boolean") selector = `${row} [role=switch][aria-checked=${attribute(String(value))}]`;
  else if (numberSteps(definition)) selector = `${row} input[aria-label=${attribute(definition.label)}]`;
  else if (definition.type === "providerLimits") selector = `${row} button:text-is(${attribute(shown)})`;
  else if (definition.type === "text" && definition.secret) selector = `${row} input[type=password][aria-label=${attribute(definition.label)}]`;
  else if (definition.type === "text") selector = `${row} button[aria-label=${attribute(definition.label)}]:text-is(${attribute(shown)})`;
  else {
    const options = definition.type === "enum" ? definition.values.map(option => ({ value: option, label: option }))
      : definition.path === "composer.shape" ? getComposerShapeOptions() : definition.options;
    const label = options.find(option => option.value === String(value))?.label ?? String(value);
    selector = `${row} :is([data-name=SelectValue],button[aria-label=${attribute(definition.label)}]):text-is(${attribute(label)})`;
  }
  const valuePattern = numberSteps(definition) ? `(?<![\\d.])${escapePattern(shown)}(?![\\d.])` : escapePattern(shown);
  return { name: "rendered setting value", terminal: { type: "screen", pattern: `${escapePattern(definition.label)}[^\\n]*${valuePattern}`, read: "visible" },
    browser: { type: "dom", selector, read: "visible" }, expected: true };
}

export function proveSettingsFixture(fixture: FixtureCase): FixtureCase {
  const behaviors = [...(fixture.behaviors ?? [])];
  const covers = fixture.covers.filter(entry => {
    if (entry.startsWith("settings:") || entry === "control:editor") { behaviors.push(entry); return false; }
    if (!entry.startsWith("setting-choice:")) return true;
    const path = entry.slice("setting-choice:".length, entry.indexOf("="));
    const value = entry.slice(entry.indexOf("=") + 1);
    const definition = settingDefinitions.find(candidate => candidate.definition.path === path)?.definition;
    const declared = definition?.type === "enum" ? definition.values.includes(value)
      : definition?.type === "submenu" || definition?.type === "multiselect" ? definition.options.some(option => option.value === value) || runtimeValues.get(path)?.has(value) === true : false;
    if (!declared) throw new Error(`Unmapped source settings choice ${entry}`);
    return true;
  });
  const path = covers.find(entry => entry.startsWith("setting:"))?.slice("setting:".length);
  const metadata = settingDefinitions.find(entry => entry.definition.path === path);
  if (!metadata) return { ...fixture, covers, behaviors };
  const definition = metadata.definition;
  const condition = metadata.setting.ui?.condition;
  for (const checkpoint of fixture.checkpoints) for (const comparison of checkpoint.comparisons) {
    for (const claim of comparison.proves ?? []) if (!covers.includes(claim.entry)) covers.push(claim.entry);
  }
  const checkpoints = fixture.checkpoints.map(checkpoint => {
    const saved = checkpoint.comparisons.find(comparison => comparison.name === "persisted setting reload");
    if (!saved) return checkpoint;
    const expected = saved.expected;
    const rendered = renderedSettingValue(definition, expected);
    const existingTarget = checkpoint.comparisons.find(comparison => comparison.name === "membership mark" || comparison.name === "ordered first member");
    const target = rendered ?? existingTarget;
    if (!target) return checkpoint;
    const numericValue: Comparison | undefined = numberSteps(definition) ? {
      name: "native numeric field value", terminal: { type: "effect", channel: `saved:${path}` },
      browser: { type: "dom", selector: `${settingRow(definition.path)} input`, read: "value" }, expected, normalize: "number",
    } : undefined;
    const assertions = [{ checkpoint: checkpoint.name, comparison: "canonical setting callback" },
      { checkpoint: checkpoint.name, comparison: "persisted setting reload" }, { checkpoint: checkpoint.name, comparison: target.name },
      ...(numericValue ? [{ checkpoint: checkpoint.name, comparison: numericValue.name }] : [])];
    const claims: BehaviorClaim[] = covers.filter(entry => entry === `setting:${path}` || entry === `control:${definition.type}`
      || entry.startsWith(`setting-choice:${path}=`) && (definition.type !== "multiselect" || Array.isArray(expected) && expected.includes(entry.slice(entry.indexOf("=") + 1))))
      .map(entry => ({ entry, effect: "interaction", value: nativeSettingValue(definition, expected), target: { checkpoint: checkpoint.name, comparison: target.name }, assertionGroups: [{ comparisons: assertions }] }));
    if (condition) for (const entry of [`setting-condition:${condition}:true`, `setting-condition:${path}:true`]) {
      if (!covers.includes(entry)) covers.push(entry);
      claims.push({ entry, effect: "interaction", value: true, target: { checkpoint: checkpoint.name, comparison: target.name },
        assertionGroups: [{ comparisons: [...assertions, { checkpoint: checkpoint.name, comparison: "canonical visibility" }] }] });
    }
    return { ...checkpoint, comparisons: [...checkpoint.comparisons.map(comparison => comparison === saved ? { ...comparison, proves: [...(comparison.proves ?? []), ...claims] } : comparison),
      ...(rendered ? [rendered] : []), ...(numericValue ? [numericValue] : []), ...(condition ? [{ name: "canonical visibility", terminal: { type: "effect" as const, channel: "visibility" }, browser: { type: "effect" as const, channel: "visibility" }, expected: true }] : [])] };
  });
  return { ...fixture, covers, behaviors, checkpoints };
}
