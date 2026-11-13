// Shared proof builders for the native prefs page (TspPrefs*) fixtures.
// Used by prefs-settings.ts, prefs-nav.ts and prefs-advisor.ts; each comparison names the
// wire properties it proves, and covers are derived from those claims.
import type { BehaviorClaim, Checkpoint, Comparison, FixtureCase } from "../scenario";
import { attribute, escapePattern } from "./settings-support";
import { settingRow } from "./settings-proofs";

export const settingsComponent = "component:packages/tui/src/overlays/settings-selector.ts#SettingsSelectorComponent";
export const advisorComponent = "component:packages/tui/src/overlays/advisor-config.ts#AdvisorConfigOverlayComponent";

export type PrefsEffect = "literal" | "layout";
export const claim = (property: string, effect: PrefsEffect, value: unknown): BehaviorClaim => ({ entry: `property:${property}`, effect, value });

/** Every claim a case proves becomes a cover, so covers can never drift from proofs. */
export function prefsCase(id: string, component: string, create: FixtureCase["create"], checkpoints: Checkpoint[]): FixtureCase {
  const entries = new Set<string>(["kind:prefs", component]);
  for (const checkpoint of checkpoints) for (const comparison of checkpoint.comparisons) for (const proof of comparison.proves ?? []) entries.add(proof.entry);
  return { id, covers: [...entries], create, checkpoints };
}

export const rowSelector = settingRow;
export const hintSelector = (path: string): string => `${settingRow(path)} p.text-muted-foreground`;
export const warningSelector = (path: string): string => `${settingRow(path)} p.text-warning`;
export const changedDotSelector = (path: string): string => `${settingRow(path)} p.font-medium span[title]`;

/** The visible text of one source scalar, read from the screen and from one DOM element. */
export function literal(name: string, terminalPattern: string, selector: string, value: string, proves: BehaviorClaim[], normalize: Comparison["normalize"] = "whitespace"): Comparison {
  return { name, terminal: { type: "screen", pattern: `(${terminalPattern})`, group: 1 }, browser: { type: "dom", selector, read: "text" }, expected: value, normalize, proves };
}

/** Like `literal`, but the DOM side reads the current value of a form field. */
export function literalFieldValue(name: string, terminalPattern: string, selector: string, value: string, proves: BehaviorClaim[]): Comparison {
  return { ...literal(name, terminalPattern, selector, value, proves), browser: { type: "dom", selector, read: "value" } };
}

/**
 * The control that carries `valuePattern` shares its row with `label`: the terminal prints the value on
 * the label's line, the browser right-aligns the control inside the same row.
 */
export function controlBesideLabel(name: string, label: string, valuePattern: string, controlSelector: string, proves: BehaviorClaim[]): Comparison {
  return {
    name,
    terminal: { type: "screen", pattern: `(?<=${escapePattern(label)}[^\\n]*?)${valuePattern}`, read: "sameRow", relativeTo: { pattern: escapePattern(label) } },
    browser: { type: "dom", selector: controlSelector, read: "rightHalf" },
    expected: true,
    proves,
  };
}

/** The label (and its changed marker) sits at the row's start: left half on both interfaces. */
export function labelAtRowStart(name: string, label: string, labelSelector: string, proves: BehaviorClaim[]): Comparison {
  return { name, terminal: { type: "screen", pattern: escapePattern(label), read: "leftHalf" }, browser: { type: "dom", selector: labelSelector, read: "leftHalf" }, expected: true, proves };
}

/** A line of text lies below another on both interfaces. */
export function belowLabel(name: string, lower: string, upper: string, lowerSelector: string, upperSelector: string, proves: BehaviorClaim[]): Comparison {
  return {
    name,
    terminal: { type: "screen", pattern: escapePattern(lower), read: "below", relativeTo: { pattern: escapePattern(upper) } },
    browser: { type: "dom", selector: lowerSelector, read: "below", relativeTo: upperSelector },
    expected: true,
    proves,
  };
}

export const controlAttribute = (path: string, selector: string): string => `${settingRow(path)} ${selector}`;
export const ariaLabel = (label: string): string => `[aria-label=${attribute(label)}]`;
