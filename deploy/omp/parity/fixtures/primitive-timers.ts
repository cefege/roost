import type { Comparison, FixtureCase } from "../scenario";
import { controlledGallery } from "./gallery-support";

const baseline = "timer baseline";
const seconds = "^([\\d.]+)(?:ms|s)";

function age(name: string, selector: string, temporal: Comparison["temporal"]): Comparison {
  return {
    name,
    terminal: { type: "screen", pattern: "elapsed ([\\d.]+)(?:ms|s)", group: 1 },
    browser: { type: "dom", selector, read: "text", pattern: seconds, group: 1 },
    normalize: "number",
    temporal,
  };
}

function remaining(temporal: Comparison["temporal"]): Comparison {
  return {
    name: "remaining seconds",
    terminal: { type: "screen", pattern: "Timed approval fixture \\((\\d+)s\\)", group: 1 },
    browser: { type: "dom", selector: 'time[data-omp-timer="countdown"]', read: "text", pattern: "^(\\d+)s", group: 1 },
    normalize: "number",
    temporal,
  };
}

const body = '[data-omp-role="omp.run.time"] time[data-omp-timer]';
const head = '[data-omp-role="omp.tool.bash"] [title="Elapsed"] > time[data-omp-timer]';

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-elapsed-live-cached-tool",
    covers: ["kind:elapsed", "property:TspElapsedProps.age", "property:TspToolProps.age", "component:packages/tui/src/chat/tool-execution.ts#ToolExecutionComponent"],
    async create(context) {
      const { ToolExecutionComponent } = await context.load<typeof import("@omp-source/packages/tui/src/chat/tool-execution")>("packages/tui/src/chat/tool-execution.ts");
      const component = new ToolExecutionComponent("bash", { command: "printf timed-fixture" }, { showImages: false }, undefined, context.tui, context.workspace);
      component.setExpanded(true);
      component.setArgsComplete();
      component.setExecutionStarted();
      return controlledGallery(context, component, {});
    },
    checkpoints: [
      {
        name: baseline,
        waitForMilliseconds: 1500,
        comparisons: [age("body elapsed seconds", body, { direction: "sample" }), age("head elapsed seconds", head, { direction: "sample" })],
      },
      {
        name: "cached accepted ages keep advancing",
        waitForMilliseconds: 1500,
        comparisons: [
          { ...age("body elapsed seconds", body, { direction: "increase", from: { checkpoint: baseline, comparison: "body elapsed seconds" } }), proves: [{ entry: "property:TspElapsedProps.age", effect: "temporal" }] },
          { ...age("head elapsed seconds", head, { direction: "increase", from: { checkpoint: baseline, comparison: "head elapsed seconds" } }), proves: [{ entry: "property:TspToolProps.age", effect: "temporal" }] },
        ],
      },
    ],
  },
  {
    id: "primitive-countdown-live-cached-dialog",
    covers: ["kind:elapsed", "property:TspElapsedProps.age", "component:packages/tui/src/chrome/countdown-timer.ts#CountdownTimer", "component:packages/tui/src/overlays/approval-dialog.ts#ApprovalDialogComponent"],
    async create(context) {
      const { ApprovalDialogComponent } = await context.load<typeof import("@omp-source/packages/tui/src/overlays/approval-dialog")>("packages/tui/src/overlays/approval-dialog.ts");
      return new ApprovalDialogComponent("Timed approval fixture\nCommand: fixture-only approval", {
        onApprove: () => context.record("approval", "approve"),
        onDeny: () => context.record("approval", "deny"),
      }, { tui: context.tui, timeout: 120_000 });
    },
    checkpoints: [
      { name: baseline, waitForMilliseconds: 1500, comparisons: [remaining({ direction: "sample" })] },
      {
        name: "cached accepted countdown keeps decreasing",
        waitForMilliseconds: 1500,
        comparisons: [{ ...remaining({ direction: "decrease", from: { checkpoint: baseline, comparison: "remaining seconds" } }), proves: [{ entry: "property:TspElapsedProps.age", effect: "temporal" }] }],
      },
    ],
  },
];
