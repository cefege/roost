// Page-mode and search-mode navigation of the real SettingsSelectorComponent: section headings,
// the page strip, and the query line, proven against the rendered TUI and DOM.
import { SETTING_TABS, TAB_METADATA, type SettingTab } from "@omp-source/packages/tui/src/overlays/settings-defs";
import type { Comparison, FixtureCase } from "../scenario";
import { createSettings, escapePattern } from "./settings-support";
import { claim, literal, literalFieldValue, prefsCase, settingsComponent } from "./prefs-support";

const page = '[data-omp-page]';
export const tab = (id: string): string => `${page} [data-omp-page-tab=${JSON.stringify(id)}] button`;
const sectionHeading = (title: string): string => `${page} section h3:text-is(${JSON.stringify(title)})`;
const rightPaneHeading = (title: string): string => `(?<=│[^│\\n]*│   )${escapePattern(title)}(?= )`;

const typeUnmatchedQuery: FixtureCase["create"] = async context => {
  const component = await createSettings(context);
  for (const character of "zzzzqq") component.handleInput(character);
  return component;
};
const queryField = `${page} input[type=search]`;

const pluginsPage = { id: "plugins", label: "Installed", icon: "plugins", group: "Plugins" };
/** The page nav the real selector describes, tab by tab, outside search (nothing is muted). */
const pageNav = [...SETTING_TABS.map(id => ({ id, label: TAB_METADATA[id].label, icon: id })), pluginsPage];
const wideTerminal = { columns: 200, rows: 40 };

const stripRow = (name: string, label: string, anchor: string, proves: Comparison["proves"]): Comparison => ({
  name,
  terminal: { type: "screen", pattern: escapePattern(label), read: "sameRow", relativeTo: { pattern: escapePattern(TAB_METADATA[anchor as SettingTab].label) } },
  browser: { type: "dom", selector: tab(label.toLowerCase()), read: "sameRow", relativeTo: tab(anchor) },
  expected: true,
  proves,
});

export const fixtureCases: FixtureCase[] = [
  prefsCase("prefs-page-appearance", settingsComponent, context => createSettings(context), [{
    name: "appearance page", comparisons: [
      literal("settings title", "(?<=╭─ )Settings(?= ─)", `${page} > h2`, "Settings", [claim("TspPrefsProps.title", "literal", "Settings")]),
      literal("section heading Composer", rightPaneHeading("Composer"), sectionHeading("Composer"), "Composer", [claim("TspPrefsSection.title", "literal", "Composer")]),
      literal("page tab Model", escapePattern("Model"), tab("model"), "Model", [claim("TspPrefsProps.pages[].label", "literal", "Model")]),
      { name: "active page content sits under its tab", terminal: { type: "screen", pattern: rightPaneHeading("Composer"), read: "below", relativeTo: { pattern: "Appearance" } },
        browser: { type: "dom", selector: sectionHeading("Composer"), read: "below", relativeTo: tab("appearance") }, expected: true, proves: [claim("TspPrefsProps.page", "layout", "appearance")] },
    ],
  }, {
    name: "wide page strip", terminalResize: wideTerminal, comparisons: [
      literal("plugins group", "Plugins", `${page} [data-omp-page-tab="plugins"] span:text-is("Plugins")`, "Plugins", [claim("TspPrefsProps.pages[].group", "literal", "Plugins")]),
      stripRow("interaction tab shares the strip", "Interaction", "appearance", [claim("TspPrefsProps.pages", "layout", pageNav), claim("TspPrefsProps.pages[].id", "layout", "interaction")]),
      { name: "tab icon leads the strip", terminal: { type: "screen", pattern: "\\S(?= Appearance)", read: "leftHalf" },
        browser: { type: "dom", selector: `${page} [data-omp-page-tab="appearance"] svg`, read: "leftHalf", relativeTo: page }, expected: true, proves: [claim("TspPrefsProps.pages[].icon", "layout", "appearance")] },
    ],
  }]),
  prefsCase("prefs-search-no-match", settingsComponent, typeUnmatchedQuery, [{
    name: "unmatched query", comparisons: [
      literalFieldValue("query text", escapePattern("zzzzqq"), queryField, "zzzzqq", [claim("TspPrefsProps.query", "literal", "zzzzqq")]),
      { name: "query caret position", terminal: { type: "screen", pattern: "zzzzqq", read: "topHalf" }, browser: { type: "dom", selector: queryField, read: "topHalf", relativeTo: page }, expected: true, proves: [claim("TspPrefsProps.cursor", "layout", 6)] },
      { name: "empty results message under the title", terminal: { type: "screen", pattern: "No matching settings", read: "below", relativeTo: { pattern: "(?<=╭─ )Settings(?= ─)" } },
        browser: { type: "dom", selector: `${page} [data-omp-page-empty]`, read: "below", relativeTo: `${page} > h2` }, expected: true, proves: [claim("TspPrefsProps.focus", "layout", null)] },
    ],
  }]),
];
