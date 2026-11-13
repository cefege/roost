// Search-mode page nav and result sections of the real SettingsSelectorComponent: matched pages lead with
// their hit counts, the rest are muted, and results group under their page, proven against the TUI and DOM.
import { slugify } from "@omp-source/packages/utils/src/slugify";
import { SETTING_TABS, TAB_METADATA } from "@omp-source/packages/tui/src/overlays/settings-defs";
import type { FixtureCase } from "../scenario";
import { createSettings, escapePattern, settingDefinitions } from "./settings-support";
import { claim, literal, prefsCase, rowSelector, settingsComponent } from "./prefs-support";
import { tab } from "./prefs-nav";

const path = "externalThinking";
const definition = settingDefinitions.find(entry => entry.definition.path === path)?.definition;
if (!definition?.group) throw new Error(`Missing grouped canonical setting ${path}`);
const { group, tab: pageId } = definition;
const row = rowSelector(path);
const page = "[data-omp-page]";
const pageHeading = `${page} [data-omp-section-page=${JSON.stringify(pageId)}]`;

/** What the real selector describes while one page matches the query: matched first with its count, the rest muted. */
const searchNav = [
  { id: pageId, label: `${TAB_METADATA[pageId].label} (1)`, icon: pageId },
  ...SETTING_TABS.filter(id => id !== pageId).map(id => ({ id, label: TAB_METADATA[id].label, icon: id, disabled: "No matches" })),
  { id: "plugins", label: "Installed", icon: "plugins", group: "Plugins", disabled: "No matches" },
];
const resultRow = JSON.parse(JSON.stringify({
  id: path, label: definition.label, hint: definition.description, warning: definition.warning, changed: true, defaultLabel: "Off",
  control: { k: "switch", on: true },
}));
const resultSections = [{ id: `${pageId}/${slugify(group)}`, title: group, page: pageId, rows: [resultRow] }];

export const fixtureCases: FixtureCase[] = [
  prefsCase("prefs-search-pages", settingsComponent, context => createSettings(context, definition, { [path]: true }), [{
    name: "search results", comparisons: [
      literal("matched page tab with count", "Model \\(1\\)", tab(pageId), `${TAB_METADATA[pageId].label} (1)`, [claim("TspPrefsProps.pages[].label", "literal", `${TAB_METADATA[pageId].label} (1)`)]),
      { name: "matched page leads the muted pages", terminal: { type: "screen", pattern: "Appearance", read: "sameRow", relativeTo: { pattern: "Model \\(1\\)" } },
        browser: { type: "dom", selector: tab("appearance"), read: "sameRow", relativeTo: tab(pageId) }, expected: true,
        proves: [claim("TspPrefsProps.pages", "layout", searchNav), claim("TspPrefsProps.pages[].id", "layout", "appearance"), claim("TspPrefsProps.pages[].disabled", "layout", "No matches")] },
      { name: "page heading sits under the title", terminal: { type: "screen", pattern: `${escapePattern(TAB_METADATA[pageId].label)}(?= +│)`, read: "below", relativeTo: { pattern: "(?<=╭─ )Settings(?= ─)" } },
        browser: { type: "dom", selector: pageHeading, read: "below", relativeTo: `${page} > h2` }, expected: true, proves: [claim("TspPrefsSection.page", "layout", pageId)] },
      { name: "result sits under the query", terminal: { type: "screen", pattern: escapePattern(definition.label), read: "below", relativeTo: { pattern: "🔍" } },
        browser: { type: "dom", selector: `${row} p.font-medium`, read: "below", relativeTo: `${page} input[type=search]` }, expected: true,
        proves: [claim("TspPrefsSection.id", "layout", `${pageId}/${slugify(group)}`), claim("TspPrefsSection.rows", "layout", [resultRow]), claim("TspPrefsProps.sections", "layout", resultSections)] },
      { name: "focused result carries the cursor mark", terminal: { type: "screen", pattern: "❯", read: "sameRow", relativeTo: { pattern: escapePattern(definition.label) } },
        browser: { type: "dom", selector: `${row}.border-ring p.font-medium`, read: "sameRow", relativeTo: `${row} [role=switch]` }, expected: true,
        proves: [claim("TspPrefsProps.focus", "layout", path)] },
    ],
  }]),
];
