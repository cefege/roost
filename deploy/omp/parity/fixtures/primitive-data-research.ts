import type { ExtensionUiComponent } from "@omp-source/packages/coding-agent/src/extensibility/extensions/types";
import type * as Dashboard from "@omp-source/packages/tui/src/apps/autoresearch-dashboard";
import type { ExperimentResult, ExperimentState } from "@omp-source/packages/tui/src/tools/autoresearch";
import type { Comparison, FixtureCase } from "../scenario";

const trend = '[data-omp-role="omp.autoresearch.trend"]';
const tally = '[data-omp-role="omp.autoresearch.tally"]';
const runs = '[data-omp-role="omp.autoresearch.runs"]';

const result = (runNumber: number, status: ExperimentResult["status"], metric: number, description: string): ExperimentResult => ({
  runNumber, commit: `c0ffee${runNumber}`, metric, metrics: { memory: 40 + runNumber }, status, description, timestamp: 1_700_000_000_000 + runNumber,
  segment: 0, confidence: null, modifiedPaths: [], scopeDeviations: [], justification: null, flagged: false, flaggedReason: null,
});

function state(): ExperimentState {
  return {
    results: [result(1, "keep", 100, "baseline"), result(2, "discard", 112, "wider batch"), result(3, "keep", 88, "pooled buffers"), result(4, "crash", 0, "unsafe cast"), result(5, "checks_failed", 95, "inline cache")],
    bestMetric: 88, bestDirection: "lower", metricName: "latency", metricUnit: "ms", secondaryMetrics: [{ name: "memory", unit: "mb" }],
    name: "Parity latency", goal: "Cut request latency", currentSegment: 0, maxExperiments: 10, confidence: 2.4, scopePaths: [], offLimits: [], constraints: [],
    notes: "", branch: "autoresearch/parity", baselineCommit: "c0ffee1", sessionId: 1,
  };
}

const sameFact = (name: string, terminal: string, selector: string, pattern: string, entry: string, value: string, read: "text" | "attribute" = "text"): Comparison => ({
  name,
  terminal: { type: "screen", pattern: terminal, group: 1 },
  browser: read === "text"
    ? { type: "dom", selector, read: "text", pattern, group: 1 }
    : { type: "dom", selector, read: "attribute", attribute: "title" },
  expected: value,
  normalize: "whitespace",
  proves: [{ entry, effect: "literal", value }],
});

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-autoresearch-trend-tally",
    covers: [
      "kind:chart", "kind:meter", "kind:table",
      "property:TspChartProps.series[].label", "property:TspMeterProps.parts[].label",
      "component:packages/tui/src/apps/autoresearch-dashboard.ts#createDashboardController",
    ],
    async create(context) {
      const { createDashboardController } = await context.load<typeof Dashboard>("packages/tui/src/apps/autoresearch-dashboard.ts");
      const runtime = { autoresearchMode: true, dashboardExpanded: true, state: state(), lastRunSummary: null, runningExperiment: null };
      let built: ExtensionUiComponent | undefined;
      const host = {
        hasUI: true,
        ui: {
          setWidget() {},
          custom<T>(factory: (tui: unknown, theme: unknown, keybindings: unknown, done: (value: T) => void) => unknown) {
            built = factory(context.tui, context.theme, {}, () => context.done()) as never;
            return new Promise<T>(() => {});
          },
        },
      };
      void createDashboardController().showOverlay(host as never, runtime);
      if (!built) throw new Error("autoresearch overlay was not built");
      return built as never;
    },
    checkpoints: [{ name: "autoresearch dashboard", comparisons: [
      sameFact("baseline run label", "Baseline: [^\\n]*?\\((#1)\\)", `${trend} span:text-matches("^#1:")`, "^(#1):", "property:TspChartProps.series[].label", "#1"),
      sameFact("best run label", "Best: [^\\n]*?\\((#3)\\)", `${trend} span:text-matches("^#3:")`, "^(#3):", "property:TspChartProps.series[].label", "#3"),
      sameFact("kept tally", "(2 kept)", `${tally} span[title="2 kept"]`, "", "property:TspMeterProps.parts[].label", "2 kept", "attribute"),
      sameFact("discarded tally", "(1 discarded)", `${tally} span[title="1 discarded"]`, "", "property:TspMeterProps.parts[].label", "1 discarded", "attribute"),
    ] }],
  },
];
