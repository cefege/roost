import { writeFileSync } from "node:fs";
import type * as PlanSave from "@omp-source/packages/tui/src/overlays/plan-save-overlay";
import type * as Reports from "@omp-source/packages/tui/src/overlays/report-panel";
import type * as TextComponent from "@omp-source/packages/tui/src/components/text";
import type * as ContextViewerModule from "@omp-source/packages/tui/src/overlays/context-viewer";
import type { ContextViewData } from "@omp-source/packages/tui/src/chrome/context-view-model";
import { callbackValue, visibleText, type FixtureCase } from "../scenario";

export const fixtureCases: FixtureCase[] = [
  ...(["commit", "cancel"] as const).map((mode): FixtureCase => ({
    id: `dialog-plan-save-${mode}`,
    covers: ["component:packages/tui/src/overlays/plan-save-overlay.ts#PlanSaveOverlay"],
    async create(context) {
      const { PlanSaveOverlay } = await context.load<typeof PlanSave>("packages/tui/src/overlays/plan-save-overlay.ts");
      return new PlanSaveOverlay("fixture-plan.md", result => {
        if (result) writeFileSync(`${context.workspace}/plan-destination.json`, JSON.stringify(result));
        context.record("destination", result ?? null); context.done();
      });
    },
    checkpoints: [
      { name: "suggested destination", comparisons: [{ name: "suggested path", terminal: { type: "screen", pattern: "(fixture-plan\\.md)", group: 1 }, browser: { type: "dom", selector: 'text="Empty saves to fixture-plan.md"', read: "text", pattern: "(fixture-plan\\.md)", group: 1 }, expected: "fixture-plan.md" }] },
      { name: "edit destination", terminalInput: "reviewed-plan.md", browserActions: [{ type: "fill", selector: "input", value: "reviewed-plan.md" }], comparisons: [{ name: "destination draft", terminal: { type: "screen", pattern: "(reviewed-plan\\.md)", group: 1 }, browser: { type: "dom", selector: "input", read: "value" }, expected: "reviewed-plan.md" }] },
      { name: mode, terminalInput: mode === "commit" ? "\r" : "\x1b", browserActions: [{ type: "click", selector: `button:text-is("${mode === "commit" ? "Save and quit" : "Cancel"}")` }], comparisons: [callbackValue("save outcome", "destination", mode === "commit" ? { path: "reviewed-plan.md" } : null), ...(mode === "commit" ? [{ name: "persisted destination", terminal: { type: "file" as const, path: "plan-destination.json", pointer: "/path" }, browser: { type: "file" as const, path: "plan-destination.json", pointer: "/path" }, expected: "reviewed-plan.md" }] : [])] },
    ],
  })),
  {
    id: "dialog-command-report-close",
    covers: ["component:packages/tui/src/overlays/report-panel.ts#ReportPanel"],
    async create(context) {
      const { ReportPanel } = await context.load<typeof Reports>("packages/tui/src/overlays/report-panel.ts");
      const { Text } = await context.load<typeof TextComponent>("packages/tui/src/components/text.ts");
      const panel = new ReportPanel({ title: "Fixture report", body: new Text("Fixture command result", 0, 0), closeKey: "escape", onClose: () => { context.record("closed", true); context.done(); } });
      panel.holdFocus(); return panel;
    },
    checkpoints: [
      { name: "report content", comparisons: [visibleText("body", "(Fixture command result)", '[data-omp-role="omp.report.body"]', "Fixture command result")] },
      { name: "close", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button:text-is("Close")' }], comparisons: [callbackValue("closed", "closed", true)] },
    ],
  },
  {
    id: "dialog-context-tool-output-open-back-close",
    covers: ["component:packages/tui/src/overlays/context-viewer.ts#ContextViewer"],
    async create(context) {
      const { ContextViewer } = await context.load<typeof ContextViewerModule>("packages/tui/src/overlays/context-viewer.ts");
      const data: ContextViewData = { breakdown: { model: undefined, contextWindow: 32000, categories: [], thresholdTokens: 28000, usedTokens: 100, freeTokens: 27900, autoCompactBufferTokens: 4000 }, categories: [{ id: "tool-output", label: "Tool Output", tokens: 100, entries: [{ id: "fixture-output", label: "Fixture read output", text: "Fixture tool output retained in context", tokens: 100, callId: "fixture-call" }] }] };
      return new ContextViewer(data, { onClose: () => { context.record("closed", true); context.done(); }, requestRender: () => context.tui.requestRender(), getRows: () => 40 });
    },
    checkpoints: [
      { name: "context overview", comparisons: [visibleText("category", "(Tool Output)", '[data-omp-node$="categories"] [data-omp-node$="tool-output"] :text-is("Tool Output")', "Tool Output")] },
      { name: "open real output entry", terminalInput: "\r", browserActions: [{ type: "click", selector: '[data-omp-node$="categories"] [data-omp-node$="tool-output"]' }, { type: "press", key: "Enter" }], comparisons: [visibleText("full output", "(Fixture tool output retained in context)", 'text="Fixture tool output retained in context"', "Fixture tool output retained in context")] },
      { name: "back to owner", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button:text-is("Back")' }], comparisons: [visibleText("category retained", "(Tool Output)", '[data-omp-node$="categories"] [data-omp-node$="tool-output"] :text-is("Tool Output")', "Tool Output")] },
      { name: "close overview", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button:text-is("Close")' }], comparisons: [callbackValue("closed", "closed", true)] },
    ],
  },
];
