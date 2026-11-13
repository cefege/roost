import type * as LoaderModule from "@omp-source/packages/tui/src/components/loader";
import type * as ShimmerModule from "@omp-source/packages/tui/src/theme/shimmer";
import type { Comparison, FixtureCase } from "../scenario";

const component = (file: string, name: string) => `component:packages/tui/src/${file}.ts#${name}`;
const workingLabel = '[data-omp-role="omp.working.label"]';
const workingStop = '[data-omp-role="omp.working.stop"]';

function rowShare(name: string, label: string, hint: string, selector: string, relativeTo: string, proves: Comparison["proves"]): Comparison {
  return {
    name,
    terminal: { type: "screen", pattern: label, read: "sameRow", relativeTo: { pattern: hint } },
    browser: { type: "dom", selector, read: "sameRow", relativeTo },
    expected: true,
    proves,
  };
}

async function workingRowLoader(context: Parameters<FixtureCase["create"]>[0], label: string, variant: "plain" | "retry", shimmerMode?: "kitt") {
  const { Loader } = await context.load<typeof LoaderModule>("packages/tui/src/components/loader.ts");
  const { shimmerText, setShimmerMode } = await context.load<typeof ShimmerModule>("packages/tui/src/theme/shimmer.ts");
  if (shimmerMode) setShimmerMode(shimmerMode);
  const animated = Object.assign((text: string) => shimmerText(text, context.theme), { animated: true as const });
  const loader = new Loader(context.tui, text => context.theme.fg("accent", text), animated, label);
  const startedAt = Date.now();
  loader.setWorkingRow(() => ({
    label,
    startedAt,
    interruptKey: "escape",
    ...(variant === "retry" ? { variant: { kind: "retry" as const, attempt: 1, max: 3, delayMs: 600_000 } } : {}),
  }), () => undefined);
  loader.setTrailer?.(() => "esc to interrupt");
  return loader;
}

const hint = "esc to interrupt";
const label = "Indexing fixture sources";
const defaultPalette = { low: "dim", mid: "muted", high: "accent" };

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-working-row-starburst",
    covers: ["kind:spinner", "kind:shimmer", "kind:elapsed", "property:TspSpinnerProps.style", "property:TspElapsedProps.format", "property:TspShimmerProps.spans", "property:TspShimmerProps.mode", "property:TspShimmerProps.palette", component("components/loader", "Loader")],
    create: context => workingRowLoader(context, label, "plain"),
    checkpoints: [{ name: "working row shares one line", comparisons: [
      rowShare("starburst spinner", label, hint, '[data-omp-node="l.spinner"]', workingStop, [{ entry: "property:TspSpinnerProps.style", effect: "layout", value: "starburst" }]),
      rowShare("elapsed clock", label, hint, '[data-omp-node="l.elapsed"]', workingStop, [{ entry: "property:TspElapsedProps.format", effect: "layout", value: "short" }]),
      rowShare("shimmering label", label, hint, workingLabel, workingStop, [
        { entry: "property:TspShimmerProps.spans", effect: "layout", value: [{ t: label, s: "muted" }] },
        { entry: "property:TspShimmerProps.mode", effect: "layout", value: "classic" },
        { entry: "property:TspShimmerProps.palette", effect: "layout", value: defaultPalette },
      ]),
    ] }],
  },
  {
    id: "primitive-working-row-retry-ring",
    covers: ["kind:meter", "property:TspMeterProps.style", "property:TspMeterProps.size", component("components/loader", "Loader")],
    create: context => workingRowLoader(context, "Retrying", "retry"),
    checkpoints: [{ name: "retry ring shares the working line", comparisons: [
      rowShare("retry countdown ring", "Retrying \\(1/3\\)", hint, '[data-omp-node="l.countdown"]', workingStop, [
        { entry: "property:TspMeterProps.style", effect: "layout", value: "ring" },
        { entry: "property:TspMeterProps.size", effect: "layout", value: "sm" },
      ]),
    ] }],
  },
  {
    id: "primitive-working-row-kitt-sweep",
    covers: ["kind:shimmer", "property:TspShimmerProps.mode", component("components/loader", "Loader")],
    create: context => workingRowLoader(context, label, "plain", "kitt"),
    checkpoints: [{ name: "kitt sweep label shares the working line", comparisons: [
      rowShare("kitt label", label, hint, workingLabel, workingStop, [{ entry: "property:TspShimmerProps.mode", effect: "layout", value: "kitt" }]),
    ] }],
  },
  {
    id: "primitive-loader-plain-row",
    covers: ["kind:spinner", "property:TspSpinnerProps.style", "property:TspRowProps.justify", "property:TspTextProps.truncate", component("components/loader", "Loader")],
    async create(context) {
      const { Loader } = await context.load<typeof LoaderModule>("packages/tui/src/components/loader.ts");
      const loader = new Loader(context.tui, text => context.theme.fg("accent", text), text => context.theme.fg("muted", text), "Scanning fixture tree");
      loader.setTrailer(() => hint);
      return loader;
    },
    checkpoints: [{ name: "plain loader line", comparisons: [
      {
        name: "loader row spans label and trailer",
        terminal: { type: "screen", pattern: "Scanning fixture tree", read: "sameRow", relativeTo: { pattern: hint } },
        browser: { type: "dom", selector: '[data-omp-role="omp.loader"]', read: "sameRow", relativeTo: '[data-omp-role="omp.loader"] > div > :last-child' },
        expected: true,
        proves: [{ entry: "property:TspRowProps.justify", effect: "layout", value: "between" }],
      },
      {
        name: "braille spinner precedes the label",
        terminal: { type: "screen", pattern: "[\u2800-\u28ff]", read: "sameRow", relativeTo: { pattern: "Scanning fixture tree" } },
        browser: { type: "dom", selector: '[data-omp-node="l.0/0"] svg', read: "sameRow", relativeTo: '[data-omp-node="l.0/message"]' },
        expected: true,
        proves: [{ entry: "property:TspSpinnerProps.style", effect: "layout", value: "braille" }],
      },
      {
        name: "trailer docks to the right",
        terminal: { type: "screen", pattern: hint, read: "rightHalf" },
        browser: { type: "dom", selector: '[data-omp-role="omp.loader"] > div > :last-child', read: "rightHalf" },
        expected: true,
        proves: [{ entry: "property:TspTextProps.truncate", effect: "layout", value: "end" }],
      },
    ] }],
  },
];
