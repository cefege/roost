import { existsSync, writeFileSync } from "node:fs";
import type * as Advisor from "@omp-source/packages/tui/src/overlays/advisor-config";
import type * as Cleanse from "@omp-source/packages/tui/src/overlays/cleanse-panel";
import type * as Board from "@omp-source/packages/tui/src/apps/cleanse-board";
import type * as History from "@omp-source/packages/tui/src/overlays/btw-history-panel";
import type { BtwHistoryRecord } from "@omp-source/packages/tui/src/overlays/btw-history";
import { modelSource } from "./pickers";
import { panelControls } from "./panel-dialogs";
import { callbackValue, visibleText, type FixtureCase } from "../scenario";

export const fixtureCases: FixtureCase[] = [
  ...(["commit", "cancel", "save-error"] as const).map((mode): FixtureCase => ({
    id: `app-advisor-config-${mode}`, covers: ["component:packages/tui/src/overlays/advisor-config.ts#AdvisorConfigOverlayComponent", "component:packages/tui/src/overlays/advisor-config.ts#EditorSheet"],
    async create(context) {
      const { AdvisorConfigOverlayComponent } = await context.load<typeof Advisor>("packages/tui/src/overlays/advisor-config.ts");
      const doc: Advisor.WatchdogConfigDoc = { advisors: [{ name: "Fixture reviewer", enabled: true, instructions: "Preserve fixture ownership" }] };
      return new AdvisorConfigOverlayComponent(context.tui, { getAvailableModels: () => [], browserSource: modelSource, defaultToolNames: new Set(["read", "grep", "glob"]), scopedModels: [], availableToolNames: ["read", "grep", "glob"] }, "project", doc, { loadDoc: async () => structuredClone(doc), save: async (scope, saved) => { if (mode === "save-error") throw new Error("Fixture advisor save refused"); const receipt = { scope, enabled: saved.advisors[0]?.enabled !== false }; writeFileSync(`${context.workspace}/advisor-config.json`, JSON.stringify(receipt)); context.record("saved", receipt); }, close: () => { context.record("closed", true); context.record("persisted", existsSync(`${context.workspace}/advisor-config.json`)); context.done(); }, requestRender: () => context.tui.requestRender(), notify: message => { context.record("notice", message); } });
    },
    checkpoints: [
      { name: "actual advisor roster", comparisons: [{ name: "reviewer", terminal: { type: "screen", pattern: "(Fixture reviewer)", group: 1 }, browser: { type: "dom", selector: "body", read: "text", pattern: "(Fixture reviewer)", group: 1 }, expected: "Fixture reviewer" }] },
      { name: "change actual enabled state", terminalInput: "\r\x1b[B\r", browserActions: [{ type: "click", selector: '[role="switch"][aria-label="Enabled"]' }], comparisons: [{ name: "actual enabled control", terminal: { type: "screen", pattern: "(Enabled)", group: 1 }, browser: { type: "dom", selector: '[role="switch"][aria-label="Enabled"]', read: "attribute", attribute: "aria-label" }, expected: "Enabled" }] },
      // A bare ESC followed by an arrow in one PTY write is one Alt+arrow chord to OMP's
      // stdin parser; CSI-u `\x1b[27u` is an unambiguous Escape with no timing hold.
      ...(mode === "cancel" ? [] : [{ name: "persist or surface save failure", terminalInput: "\x1b[27u\x1b[B\x1b[B\x1b[B\x1b[B\r", browserActions: [{ type: "click" as const, selector: 'button:text-is("Save")' }], comparisons: mode === "save-error" ? [callbackValue("save failure", "notice", "Advisor config: Fixture advisor save refused")] : [callbackValue("saved state", "saved", { scope: "project", enabled: false }), { name: "persisted disabled state", terminal: { type: "file" as const, path: "advisor-config.json", pointer: "/enabled" }, browser: { type: "file" as const, path: "advisor-config.json", pointer: "/enabled" }, expected: false }] }]),
      { name: "close configuration owner", terminalInput: mode === "cancel" ? "\x1b\x1b" : "\x1b", browserActions: [{ type: "press", key: "Escape" }], comparisons: [callbackValue("closed", "closed", true), callbackValue("only successful Save persists", "persisted", mode === "commit")] },
    ],
  })),
  ...(["clean", "cancelled", "error"] as const).map((mode): FixtureCase => ({
    id: `app-cleanse-panel-${mode}`, covers: ["component:packages/tui/src/overlays/cleanse-panel.ts#CleansePanelComponent", "component:packages/tui/src/apps/cleanse-board.ts#CleanseBoardModel", "kind:meter", "kind:agent", "property:TspAgentProps.name"],
    behaviors: ["controller:cleanse-settlement", `controller:${mode}`],
    async create(context) {
      const { CleansePanelComponent } = await context.load<typeof Cleanse>("packages/tui/src/overlays/cleanse-panel.ts"); const { CleanseBoardModel } = await context.load<typeof Board>("packages/tui/src/apps/cleanse-board.ts");
      const first: Board.CleanseAssignment = { index: 0, groups: [{ file: "alpha.ts" }], weight: 1 }; const second: Board.CleanseAssignment = { index: 1, groups: [{ file: "beta.ts" }], weight: 1 };
      const panel = new CleansePanelComponent({ model: new CleanseBoardModel(), request: "Fixture repair", tui: context.tui }); panel.agentStarted("fixture-one", first); panel.agentStarted("fixture-two", second); panel.agentFinished({ name: "fixture-one", success: true }, first);
      return panelControls(context, panel, { "Finish fixture repair": () => { if (mode === "error") panel.markError("Fixture repair refused"); else { panel.agentFinished({ name: "fixture-two", success: mode === "clean", error: mode === "cancelled" ? "Fixture repair cancelled" : undefined }, second); panel.finish(mode); } context.record("outcome", mode); } });
    },
    checkpoints: [
      { name: "real live repair progress", comparisons: [{ name: "half-complete lanes", terminal: { type: "screen", pattern: "(1/2)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.cleanse.meter"]', read: "text", pattern: "(1/2)", group: 1 }, expected: "1/2" }, { name: "active lane identity", terminal: { type: "screen", pattern: "(fixture-two)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.cleanse.lane"]', read: "text", pattern: "(fixture-two)", group: 1 }, expected: "fixture-two", proves: [{ entry: "property:TspAgentProps.name", effect: "literal", value: "fixture-two" }] }] },
      { name: "settle actual board", terminalInput: "\r", browserActions: [{ type: "click", selector: 'text="Finish fixture repair"' }], comparisons: [callbackValue("outcome", "outcome", mode), { name: "settled information", terminal: { type: "screen", pattern: mode === "clean" ? "(Clean)" : mode === "cancelled" ? "(Cancelled)" : "(Fixture repair refused)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.overlay.cleanse"]', read: "text", pattern: mode === "clean" ? "(Clean)" : mode === "cancelled" ? "(Cancelled)" : "(Fixture repair refused)", group: 1 }, expected: mode === "clean" ? "Clean" : mode === "cancelled" ? "Cancelled" : "Fixture repair refused" }] },
      { name: "close fixture owner", terminalInput: "\x1b", browserActions: [{ type: "press", key: "Escape" }], comparisons: [callbackValue("closed", "closed", true)] },
    ],
  })),
  ...(["followup", "cancel-followup", "stop"] as const).map((mode): FixtureCase => ({
    id: `app-btw-history-${mode}`, covers: ["component:packages/tui/src/overlays/btw-history-panel.ts#BtwHistoryPanel"],
    async create(context) {
      const { BtwHistoryPanel } = await context.load<typeof History>("packages/tui/src/overlays/btw-history-panel.ts");
      let record: BtwHistoryRecord = { id: "fixture-topic", leafId: null, question: "Fixture side question", answer: "Fixture retained answer", status: mode === "stop" ? "running" : "complete", createdAt: 1794000000000, updatedAt: 1794000000000 };
      const panel = new BtwHistoryPanel({ records: [record], escapeHides: true, onClose: () => { context.record("closed", true); context.record("followups", record.followUps?.length ?? 0); context.done(); }, onCopy: current => context.record("copied", current.followUps?.at(-1)?.answer ?? current.answer), onCancel: current => { record = { ...current, status: "cancelled" }; panel.update([record]); context.record("cancelled", current.id); }, canFollowUp: () => true, onFollowUp: async (current, question, signal) => { if (signal.aborted) return false; record = { ...current, followUps: [{ question, answer: "Fixture followup answer", status: "complete", createdAt: 1794000001000, updatedAt: 1794000001000 }] }; panel.update([record]); writeFileSync(`${context.workspace}/btw-history.json`, JSON.stringify(record)); context.record("followup", question); return true; }, requestRender: () => context.tui.requestRender(), getHeight: () => 30 }); return panel;
    },
    checkpoints: [
      { name: "real history answer", comparisons: [visibleText("answer", "(Fixture retained answer)", '[data-omp-role="omp.btw.reader"] p', "Fixture retained answer")] },
      ...(mode === "stop" ? [{ name: "cancel running answer", terminalInput: "x", browserActions: [{ type: "click" as const, selector: 'button:text-is("Stop answer")' }], comparisons: [callbackValue("cancel identity", "cancelled", "fixture-topic"), { name: "retained cancellation information", terminal: { type: "screen" as const, pattern: "(Cancelled)", group: 1 }, browser: { type: "dom" as const, selector: '[data-omp-role="omp.btw.reader"]', read: "text" as const, pattern: "(Cancelled)", group: 1 }, expected: "Cancelled" }] }] : [
        { name: "copy without promoting conversation", terminalInput: "c", browserActions: [{ type: "click" as const, selector: 'button:text-is("Copy answer")' }], comparisons: [callbackValue("copied answer", "copied", "Fixture retained answer")] },
        { name: "compose real followup", terminalInput: "fFixture followup question", browserActions: [{ type: "click" as const, selector: 'button:text-is("Follow up")' }, { type: "fill" as const, selector: '[data-omp-role="omp.btw.composer"] input', value: "Fixture followup question" }], comparisons: [{ name: "followup draft", terminal: { type: "screen" as const, pattern: "(Fixture followup question)", group: 1 }, browser: { type: "dom" as const, selector: '[data-omp-role="omp.btw.composer"] input', read: "value" as const }, expected: "Fixture followup question" }] },
        { name: mode === "followup" ? "commit followup" : "cancel nested composer", terminalInput: mode === "followup" ? "\r" : "\x1b", browserActions: [{ type: "click" as const, selector: mode === "followup" ? 'button:text-is("Send")' : 'button:text-is("Cancel")' }], comparisons: mode === "followup" ? [callbackValue("followup", "followup", "Fixture followup question"), visibleText("new answer", "(Fixture followup answer)", '[data-omp-role="omp.btw.reader"] p:text-is("Fixture followup answer")', "Fixture followup answer")] : [visibleText("owner retained", "(Fixture retained answer)", '[data-omp-role="omp.btw.reader"] p', "Fixture retained answer")] },
      ]),
      { name: "close owning history", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button:text-is("Close")' }], comparisons: [callbackValue("closed", "closed", true), callbackValue("committed followups", "followups", mode === "followup" ? 1 : 0)] },
    ],
  })),
];
