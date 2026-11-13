// Advisor configuration prefs pages built by the real AdvisorConfigOverlayComponent; the detail screen
// exercises the switch, text, action, segmented, stepper, menu and multi controls of one advisor.
import type * as Advisor from "@omp-source/packages/tui/src/overlays/advisor-config";
import type { BehaviorClaim, FixtureCase } from "../scenario";
import { modelSource } from "./pickers";
import { escapePattern } from "./settings-support";
import { advisorComponent, claim, controlBesideLabel, literal, prefsCase } from "./prefs-support";

const enterDetail = "\r";
const page = "[data-omp-page]";
const row = (id: string): string => `${page} [data-omp-setting=${JSON.stringify(id)}]`;

function createAdvisor(configure: (doc: Advisor.WatchdogConfigDoc) => void): FixtureCase["create"] {
  return async context => {
    const { AdvisorConfigOverlayComponent } = await context.load<typeof Advisor>("packages/tui/src/overlays/advisor-config.ts");
    const doc: Advisor.WatchdogConfigDoc = {
      instructions: "Shared fixture guidance",
      advisors: [{ name: "Fixture reviewer", enabled: true, instructions: "Preserve fixture ownership", reviewMode: "agent-end", reviewInterval: 3, syncBacklog: "3", tools: ["read", "grep"] }],
    };
    configure(doc);
    return new AdvisorConfigOverlayComponent(
      context.tui,
      { getAvailableModels: () => [], browserSource: modelSource, defaultToolNames: new Set(["read", "grep", "glob"]), scopedModels: [], availableToolNames: ["read", "grep", "glob"] },
      "project", doc,
      { loadDoc: async () => structuredClone(doc), save: async () => undefined, close: () => undefined, requestRender: () => undefined, notify: () => undefined },
    );
  };
}

const modelSelector = "parity/advisor-model:high";
const detailRow = (label: string, id: string, valuePattern: string, controlSelector: string, claims: BehaviorClaim[]) =>
  controlBesideLabel(`${label} beside label`, label, valuePattern, `${row(id)} ${controlSelector}`, claims);

export const fixtureCases: FixtureCase[] = [
  prefsCase("prefs-advisor-detail", advisorComponent, createAdvisor(doc => { const [first] = doc.advisors; if (first) first.model = modelSelector; }), [{
    name: "advisor detail", terminalInput: enterDetail, comparisons: [
      literal("advisor name value", "(?<=Name {2,})Fixture reviewer", `${row("name")} button`, "Fixture reviewer", [claim("TspPrefsControl.value", "literal", "Fixture reviewer")]),
      literal("model label", `(?<=Model {2,})${escapePattern(modelSelector)}`, `${row("model")} button`, modelSelector, [claim("TspPrefsControl.label", "literal", modelSelector)]),
      detailRow("Review interval", "reviewInterval", "3", "input[type=number]", [
        claim("TspPrefsControl.k", "layout", "number"), claim("TspPrefsControl.min", "layout", 1), claim("TspPrefsControl.step", "layout", 1)]),
      detailRow("Review mode", "reviewMode", "agent-end", "[role=tablist]", [claim("TspPrefsControl.style", "layout", "segmented")]),
      detailRow("Sync backlog", "syncBacklog", "3", "button", [claim("TspPrefsControl.style", "layout", "menu")]),
      detailRow("Tools", "tools", "read, grep", "[role=checkbox]", [claim("TspPrefsControl.values", "layout", ["read", "grep"])]),
    ],
  }]),
];
