import type * as JobsPanel from "@omp-source/packages/tui/src/overlays/jobs-panel";
import type { JobWidgetView } from "@omp-source/packages/tui/src/tools/job-widget";
import type { Comparison, FixtureCase } from "../scenario";
import { contentComparison } from "./gallery-support";
import { escapePattern, layoutClaims, toneRgb } from "./primitive-data-support";

type Tone = "accent" | "success" | "error" | "warning" | "muted" | "info" | "pending" | "neutral" | "user";
interface SparkMark { tone: Tone; value: number }
interface Spark { label: string; last: string; lastValue: number; min: number; max: number; tone: Tone; token: string; marks: SparkMark[]; unit: string }

const sparkRow = (spark: Spark) => `[data-omp-role="omp.job.widget.spark"]:has([title^="${markLabel(spark.marks[0]!)}:"])`;
const markLabel = (mark: SparkMark): string => `${mark.tone} ${mark.value}`;

const sparks: Spark[] = [
  { label: "decode", last: "186 tok/s", lastValue: 186, unit: "tok/s", min: 0, max: 200, tone: "accent", token: "accent", marks: [{ tone: "success", value: 150 }, { tone: "error", value: 60 }, { tone: "accent", value: 100 }] },
  { label: "latency", last: "212 ms", lastValue: 212, unit: "ms", min: 0, max: 300, tone: "warning", token: "warning", marks: [{ tone: "warning", value: 250 }, { tone: "muted", value: 40 }, { tone: "info", value: 120 }] },
  { label: "queue", last: "31 jobs", lastValue: 31, unit: "jobs", min: 0, max: 50, tone: "user", token: "customMessageLabel", marks: [{ tone: "pending", value: 30 }, { tone: "neutral", value: 10 }, { tone: "user", value: 45 }] },
];

const values = (spark: Spark): number[] => Array.from({ length: 12 }, (_, index) => index === 11 ? spark.lastValue : Math.round(spark.max * (0.3 + 0.04 * index)));
const tip = (spark: Spark, value: number, index: number): string => `t+${index} · ${value} ${spark.unit}`;

const view = (now: number): JobWidgetView => ({
  title: "verb-bench · 3 models",
  stage: "Warming cache",
  headline: { text: "170 tok/s", label: "avg", trend: { text: "▲ 15%", tone: "success" } },
  state: "live",
  updatedAt: now,
  blocks: [
    ...sparks.map(spark => ({
      kind: "spark" as const,
      label: spark.label,
      values: values(spark),
      min: spark.min,
      max: spark.max,
      marks: spark.marks.map(mark => ({ value: mark.value, label: markLabel(mark), tone: mark.tone })),
      last: spark.last,
      tips: values(spark).map((value, index) => tip(spark, value, index)),
      tone: spark.tone,
    })),
    { kind: "bars" as const, label: "tests", max: 9, items: [{ label: "alpha", value: 7, text: "7/9", tone: "success" as const }, { label: "beta", value: 3, text: "3/9", tone: "warning" as const }, { label: "gamma", value: 1, text: "1/9", tone: "error" as const }] },
    { kind: "meter" as const, label: "trials", value: 0.78, text: "7/9 trials", tone: "accent" as const },
  ],
});

function sparkComparisons(spark: Spark): Comparison[] {
  const row = sparkRow(spark);
  const chart = `${row} [data-omp-node$="chart"]`;
  const series = values(spark).map((value, index) => ({ value, title: tip(spark, value, index) }));
  const marks = spark.marks.map(mark => ({ value: mark.value, tone: mark.tone, title: markLabel(mark) }));
  const lastTitle = `${chart} [title$=": ${spark.lastValue}"]`;
  const comparisons: Comparison[] = [
    {
      name: `${spark.label} chart shares the row of its latest value`,
      terminal: { type: "screen", pattern: spark.last, relativeTo: { pattern: spark.label }, read: "sameRow" },
      browser: { type: "dom", selector: chart, relativeTo: `${row} [data-omp-role="omp.job.widget.side"] :text-is(${JSON.stringify(spark.last)})`, read: "sameRow" },
      expected: true,
      proves: layoutClaims("TspChartProps", { kind: "bars", size: "md", min: spark.min, max: spark.max, token: spark.token, summary: `${spark.label} ${spark.last}`, series, marks }),
    },
    {
      name: `${spark.label} latest series value`,
      terminal: { type: "screen", pattern: `(${spark.lastValue}) ${escapePattern(spark.unit)}`, group: 1 },
      browser: { type: "dom", selector: lastTitle, read: "attribute", attribute: "title", pattern: `: (${spark.lastValue})$`, group: 1 },
      expected: String(spark.lastValue),
      normalize: "whitespace",
      proves: [{ entry: "property:TspChartProps.series[].value", effect: "literal", value: spark.lastValue }],
    },
  ];
  for (const mark of spark.marks) {
    const label = markLabel(mark);
    const selector = `${row} [title^="${label}:"]`;
    const rgb = toneRgb[mark.tone];
    comparisons.push(
      {
        name: `${label} mark title`,
        terminal: { type: "screen", pattern: `┄ (${escapePattern(label)})`, group: 1 },
        browser: { type: "dom", selector, read: "attribute", attribute: "title", pattern: `^(${escapePattern(label)}):`, group: 1 },
        expected: label,
        normalize: "whitespace",
        proves: [{ entry: "property:TspChartMark.title", effect: "literal", value: label }],
      },
      {
        name: `${label} mark value`,
        terminal: { type: "screen", pattern: `${mark.tone} (${mark.value})`, group: 1 },
        browser: { type: "dom", selector, read: "attribute", attribute: "title", pattern: `: (${mark.value})$`, group: 1 },
        expected: String(mark.value),
        normalize: "whitespace",
        proves: [{ entry: "property:TspChartMark.value", effect: "literal", value: mark.value }],
      },
      {
        name: `${label} mark paint`,
        terminal: { type: "screen", pattern: `(┄) ${escapePattern(label)}`, group: 1, read: "foreground" },
        browser: { type: "dom", selector, read: "css", property: "border-top-color" },
        ...(rgb ? { expected: rgb } : {}),
        proves: [{ entry: "property:TspChartMark.tone", effect: "appearance", value: mark.tone }],
      },
    );
  }
  return comparisons;
}

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-jobs-widget-chart-meter",
    covers: [
      "kind:chart", "kind:meter",
      "property:TspChartProps.kind", "property:TspChartProps.size", "property:TspChartProps.min", "property:TspChartProps.max",
      "property:TspChartProps.series", "property:TspChartProps.series[].value", "property:TspChartProps.marks", "property:TspChartProps.token", "property:TspChartProps.summary",
      "property:TspChartMark.title", "property:TspChartMark.value", "property:TspChartMark.tone",
      "property:TspMeterProps.label", "property:TspMeterProps.size",
      "component:packages/tui/src/overlays/jobs-panel.ts#JobsSheet",
    ],
    async create(context) {
      const { JobsSheet } = await context.load<typeof JobsPanel>("packages/tui/src/overlays/jobs-panel.ts");
      const job: JobsPanel.JobsPanelJob = { id: "widget-job", type: "bash", label: "verb-bench", intent: "Bench three models", caption: "Streams throughput", status: "running", startTime: 1794000000000 };
      return new JobsSheet({
        load: () => ({ running: [job], recent: [] }),
        inspect: () => ({ command: "verb-bench --models 3", cwd: context.workspace, pids: [], output: "bench output line", widget: view(1794000000000) }),
        cancel: () => undefined,
        close: () => { context.record("close", true); context.done(); },
      });
    },
    checkpoints: [{
      name: "live widget blocks",
      comparisons: [
        ...sparks.flatMap(sparkComparisons),
        contentComparison("meter label", "(7/9 trials)", `[data-omp-role="omp.job.widget.meter"] :text-is("7/9 trials")`, "7/9 trials", [{ entry: "property:TspMeterProps.label", effect: "literal", value: "7/9 trials" }]),
        {
          name: "meter shares the row of its caption",
          terminal: { type: "screen", pattern: "7/9 trials", relativeTo: { pattern: "trials" }, read: "sameRow" },
          browser: { type: "dom", selector: `[data-omp-role="omp.job.widget.meter"] [role="meter"]`, relativeTo: `[data-omp-role="omp.job.widget.meter"] :text-is("trials")`, read: "sameRow" },
          expected: true,
          proves: layoutClaims("TspMeterProps", { size: "sm", label: "7/9 trials" }),
        },
      ],
    }],
  },
];
