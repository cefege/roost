//! Comparison builders shared by the picker proof scenarios: exact-scalar literals, pairwise layout
//! relations and paint/style appearance, each tied to the TspPicker* entries it proves.
import type { Comparison } from "../scenario";

export type Claim = readonly [entry: string, value: unknown];
export const picker = (member: string) => `property:TspPicker${member}`;

/** Every TspPicker entry a claim list proves, for a scenario's `covers`. */
export const coversFor = (...lists: Claim[][]) => ["kind:picker", ...new Set(lists.flat().map(([entry]) => picker(entry)))];
/** The claim list minus entries a scenario proves through a more specific comparison. */
export const without = (list: Claim[], ...entries: string[]) => list.filter(([entry]) => !entries.includes(entry));

/** The single claim a list holds for `entry`. */
export const claimOf = (list: Claim[], entry: string): Claim => {
  const found = list.find(([name]) => name === entry);
  if (!found) throw new Error(`Golden picker claims lack ${entry}`);
  return found;
};

const claims = (list: Claim[], effect: "literal" | "layout" | "appearance") => list.map(([entry, value]) => ({ entry: picker(entry), effect, value }));

export interface BrowserText { selector: string; pattern?: string; group?: number; read?: "text" | "value"; attribute?: string }

/** The same visible scalar read from the terminal screen and the DOM, claimed for every listed entry. */
export function literal(name: string, terminalPattern: string, browser: BrowserText, expected: string | number, entries: Claim[], normalize: "whitespace" | "number" = "whitespace"): Comparison {
  const read = browser.attribute ? { read: "attribute" as const, attribute: browser.attribute } : { read: browser.read ?? "text" };
  return {
    name, terminal: { type: "screen", pattern: terminalPattern, group: 1 },
    browser: { type: "dom", selector: browser.selector, ...read, ...(browser.pattern ? { pattern: browser.pattern, group: browser.group ?? 1 } : {}) },
    expected, normalize, proves: claims(entries, "literal"),
  };
}

/** `subject` renders below `anchor` on both adapters. */
export function below(name: string, subject: { terminal: string; browser: string }, anchor: { terminal: string; browser: string }, entries: Claim[]): Comparison {
  return {
    name, terminal: { type: "screen", pattern: subject.terminal, relativeTo: { pattern: anchor.terminal }, read: "below" },
    browser: { type: "dom", selector: subject.browser, relativeTo: anchor.browser, read: "below" }, expected: true, proves: claims(entries, "layout"),
  };
}

/** `subject` shares a rendered row with `anchor` on both adapters. */
export function sameRow(name: string, subject: { terminal: string; browser: string }, anchor: { terminal: string; browser: string }, entries: Claim[]): Comparison {
  return {
    name, terminal: { type: "screen", pattern: subject.terminal, relativeTo: { pattern: anchor.terminal }, read: "sameRow" },
    browser: { type: "dom", selector: subject.browser, relativeTo: anchor.browser, read: "sameRow" }, expected: true, proves: claims(entries, "layout"),
  };
}
