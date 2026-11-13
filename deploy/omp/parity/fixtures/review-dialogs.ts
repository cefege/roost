import { writeFileSync } from "node:fs";
import type * as Review from "@omp-source/packages/tui/src/overlays/plan-review-overlay";
import type * as Annotation from "@omp-source/packages/tui/src/overlays/annotation-overlay";
import type * as Move from "@omp-source/packages/tui/src/overlays/move-overlay";
import { callbackValue, visibleText, type FixtureCase } from "../scenario";

export const fixtureCases: FixtureCase[] = [
  ...(["commit", "cancel", "copy"] as const).map((mode): FixtureCase => ({
    id: `dialog-plan-review-${mode}`, covers: ["component:packages/tui/src/overlays/plan-review-overlay.ts#PlanReviewOverlay"],
    async create(context) {
      const { PlanReviewOverlay } = await context.load<typeof Review>("packages/tui/src/overlays/plan-review-overlay.ts");
      return new PlanReviewOverlay("# Fixture review\n\nPreserve fixture state.", { options: ["Fixture approve", "Fixture revise"], promptTitle: "Fixture decision" }, { onPick: label => { writeFileSync(`${context.workspace}/review-choice.json`, JSON.stringify({ label })); context.record("pick", label); context.done(); }, onCancel: () => { context.record("cancel", true); context.done(); }, onCopyPlan: content => { context.record("copy", content); } });
    },
    checkpoints: [
      { name: "actual plan content", comparisons: [visibleText("plan body", "(Preserve fixture state\\.)", '[data-omp-role="omp.plan.body"] p', "Preserve fixture state.")] },
      ...(mode === "copy" ? [{ name: "copy actual source plan", terminalInput: "c", browserActions: [{ type: "click" as const, selector: 'button:text-is("Copy")' }], comparisons: [callbackValue("copied plan", "copy", "# Fixture review\n\nPreserve fixture state.\n")] }] : []),
      { name: mode === "commit" ? "commit decision" : "cancel", terminalInput: mode === "commit" ? "\r" : "\x1b", browserActions: mode === "commit" ? [{ type: "doubleClick", selector: '[data-omp-role="omp.plan.options"] [data-omp-node$="o0"]' }] : [{ type: "click", selector: 'button:text-is("Cancel")' }], comparisons: mode === "commit" ? [callbackValue("decision", "pick", "Fixture approve"), { name: "committed selection", terminal: { type: "file", path: "review-choice.json", pointer: "/label" }, browser: { type: "file", path: "review-choice.json", pointer: "/label" }, expected: "Fixture approve" }] : [callbackValue("cancel", "cancel", true)] },
    ],
  })),
  ...(["commit", "cancel"] as const).map((mode): FixtureCase => ({
    id: `dialog-annotation-${mode}`, covers: ["component:packages/tui/src/overlays/annotation-overlay.ts#AnnotationOverlay"],
    async create(context) {
      const { AnnotationOverlay } = await context.load<typeof Annotation>("packages/tui/src/overlays/annotation-overlay.ts");
      return new AnnotationOverlay(context.tui, context.theme, context.keybindings, { id: "fixture-text", kind: "quote", label: "Fixture source", text: "Fixture first line\nFixture second line" }, { onComplete: result => { const receipt = result ?? null; writeFileSync(`${context.workspace}/annotations.json`, JSON.stringify(receipt)); context.record("annotation", receipt); context.done(); } });
    },
    checkpoints: [
      { name: "real reviewed text", comparisons: [{ name: "first source line", terminal: { type: "screen", pattern: "(Fixture first line)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.review.text"]', read: "text", pattern: "(Fixture first line)", group: 1 }, expected: "Fixture first line" }] },
      ...(mode === "commit" ? [
        { name: "annotate selected source line", terminalInput: "aFixture line note", browserActions: [{ type: "doubleClick" as const, selector: '[data-omp-role="omp.review.text"] [data-omp-node$="l0"]' }, { type: "fill" as const, selector: '[data-omp-role="omp.review.composer"] textarea', value: "Fixture line note" }], comparisons: [{ name: "real note draft", terminal: { type: "screen" as const, pattern: "(Fixture line note)", group: 1 }, browser: { type: "dom" as const, selector: '[data-omp-role="omp.review.composer"] textarea', read: "value" as const }, expected: "Fixture line note" }] },
        { name: "save note without committing owner", terminalInput: "\r", browserActions: [{ type: "press" as const, key: "Enter", selector: '[data-omp-role="omp.review.composer"] textarea' }], comparisons: [{ name: "saved line note", terminal: { type: "screen" as const, pattern: "(Fixture line note)", group: 1 }, browser: { type: "dom" as const, selector: '[data-omp-role="omp.review.text"]', read: "text" as const, pattern: "(Fixture line note)", group: 1 }, expected: "Fixture line note" }] },
      ] : []),
      { name: "finish owner", terminalInput: mode === "commit" ? "\t\r" : "\x1b", browserActions: [{ type: "click", selector: mode === "commit" ? 'button:text-is("Paste into prompt")' : 'button:text-is("Cancel")' }], comparisons: [callbackValue("annotation result", "annotation", mode === "commit" ? { action: "paste", annotations: [{ scope: "line", line: 1, quote: "Fixture first line", note: "Fixture line note" }] } : null)] },
    ],
  })),
  ...(["commit", "cancel"] as const).map((mode): FixtureCase => ({
    id: `dialog-move-${mode}`, covers: ["component:packages/tui/src/overlays/move-overlay.ts#MoveOverlay"],
    async create(context) {
      const { MoveOverlay } = await context.load<typeof Move>("packages/tui/src/overlays/move-overlay.ts");
      const entries = [{ label: "alpha/", value: `${context.workspace}/alpha` }, { label: "beta/", value: `${context.workspace}/beta` }];
      return new MoveOverlay(context.workspace, result => { const destination = result?.directory.split("/").at(-1) ?? null; context.record("destination", destination); context.done(); }, { search: prefix => entries.filter(entry => entry.label.includes(prefix)) });
    },
    checkpoints: [
      { name: "real destination list", comparisons: [{ name: "candidate directory", terminal: { type: "screen", pattern: "(alpha/)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.dialog.move"]', read: "text", pattern: "(alpha/)", group: 1 }, expected: "alpha/" }] },
      { name: "search destination", terminalInput: "beta", browserActions: [{ type: "fill", selector: "input", value: "beta" }], comparisons: [{ name: "filtered directory", terminal: { type: "screen", pattern: "(beta/)", group: 1 }, browser: { type: "dom", selector: '[data-omp-node$="results"]', read: "text", pattern: "(beta/)", group: 1 }, expected: "beta/" }] },
      { name: "finish destination owner", terminalInput: mode === "commit" ? "\r" : "\x1b", browserActions: [{ type: "click", selector: mode === "commit" ? 'button:text-is("Confirm")' : 'button:text-is("Cancel")' }], comparisons: [callbackValue("destination", "destination", mode === "commit" ? "beta" : null)] },
    ],
  })),
];
