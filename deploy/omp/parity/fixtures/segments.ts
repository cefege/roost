import { mkdir } from "node:fs/promises";
import type * as TerminalCapabilitiesModule from "@omp-source/packages/tui/src/terminal-capabilities";
import type { SegmentContext } from "@omp-source/packages/tui/src/status-line/types";
import type { Comparison, FixtureCase } from "../scenario";
import { callbackValue } from "../scenario";
import { assertRegistry, contentComparison, controlledGallery, linkComparison } from "./gallery-support";
import { segmentSpecs, type SegmentSpec } from "./segments-data";
import { fixtureCases as statusLineCases } from "./status-line";

const escapePattern = (value: string) => value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

function spanLiteral(id: string, value: string): Comparison {
  return {
    name: "exact production span",
    terminal: { type: "screen", pattern: `(${escapePattern(value)})`, group: 1 },
    browser: { type: "dom", selector: `[data-omp-role="parity.segment.${id}"] :is(span,a):text-is(${JSON.stringify(value)}):not(:has(:is(span,a)))`, read: "text" },
    expected: value,
    proves: [{ entry: "property:TspSpan.t", effect: "literal", value }],
  };
}

function rateLiterals(expected: string): Comparison[] {
  const selector = '[data-omp-role="parity.segment.token_rate"] [data-omp-node]';
  return [
    {
      name: "exact production rate value",
      terminal: { type: "screen", pattern: "(\\d+\\.\\d+) tok/s", group: 1 },
      browser: { type: "dom", selector, read: "text", pattern: "^(\\d+\\.\\d+) tok/s$", group: 1 },
      normalize: "number", expected: Number.parseFloat(expected),
      proves: [{ entry: "property:TspRateProps.value", effect: "literal", value: Number.parseFloat(expected) }],
    },
    {
      name: "exact production rate unit",
      terminal: { type: "screen", pattern: "\\d+\\.\\d+ (tok/s)", group: 1 },
      browser: { type: "dom", selector, read: "text", pattern: "^\\d+\\.\\d+ (tok/s)$", group: 1 },
      expected: "tok/s",
      proves: [{ entry: "property:TspRateProps.unit", effect: "literal", value: "tok/s" }],
    },
  ];
}
const singleStat = "singleStatSegment";
const segmentComponents: Readonly<Record<string, string>> = {
  ...Object.fromEntries(segmentSpecs.map(spec => [spec.id, `${spec.id.replace(/_([a-z])/g, (_, letter: string) => letter.toUpperCase())}Segment`])),
  token_in: singleStat, token_out: singleStat, cache_read: singleStat, cache_write: singleStat,
};

const scratchProjectDirectory = "/tmp/parity-segment-path";
const sentinelId = "hostname";
const sentinelText = "gallery-host";

interface ResolvedVariant {
  kind: "visible" | "live" | "hidden";
  expected?: string;
  spanValue?: string;
  pattern?: string;
  projectDirectory?: boolean;
  apply(context: SegmentContext, index: number): void;
}

const resolveVariants = (spec: SegmentSpec): ResolvedVariant[] => [
  ...spec.expected.map((expected, index): ResolvedVariant => ({ kind: "visible", expected, spanValue: spec.spanValues?.[index], apply: context => spec.configure(context, index) })),
  ...(spec.extra ?? []).map((extra): ResolvedVariant => extra.kind === "visible"
    ? { kind: "visible", expected: extra.expected, spanValue: extra.spanValue, projectDirectory: extra.projectDirectory, apply: extra.configure }
    : extra.kind === "live" ? { kind: "live", pattern: extra.pattern, apply: extra.configure } : { kind: "hidden", apply: extra.configure }),
];

/** The text the hidden segment would have shown in an earlier state; its absence is the hidden evidence. */
const absentPattern = (spec: SegmentSpec): string =>
  `(?:${spec.expected.map(value => `(?<!\\w)${escapePattern(value)}(?!\\w)`).join("|")})`;

const hiddenPattern = (spec: SegmentSpec): string => `(?<![^\\n]*${absentPattern(spec)})(${sentinelText})(?![^\\n]*${absentPattern(spec)})`;


function hiddenComparisons(spec: SegmentSpec, index: number): Comparison[] {
  const pattern = hiddenPattern(spec);
  return [
    {
      name: "hidden segment leaves only its neighbour",
      terminal: { type: "screen", pattern, group: 1 },
      browser: { type: "dom", selector: '[data-omp-role="parity.segment.bar"]', read: "text", pattern, group: 1 },
      expected: sentinelText, normalize: "whitespace",
    },
    {
      name: "hidden segment production visibility",
      terminal: { type: "effect", channel: "segment-visibility" },
      browser: { type: "effect", channel: "segment-visibility" },
      expected: { rendered: false, described: false },
    },
    callbackValue("context mutation applied", "segment-variant", index),
  ];
}

function liveComparisons(spec: SegmentSpec, pattern: string, index: number): Comparison[] {
  return [
    {
      name: "running segment shows live seconds",
      terminal: { type: "screen", pattern, group: 1 },
      browser: { type: "dom", selector: `[data-omp-role="parity.segment.${spec.id}"]`, read: "text", pattern, group: 1 },
      normalize: "number", temporal: { direction: "sample" },
    },
    callbackValue("context mutation applied", "segment-variant", index),
  ];
}

function visibleComparisons(spec: SegmentSpec, expected: string, spanValue: string | undefined, index: number): Comparison[] {
  return [
    contentComparison("rendered segment value", `(${escapePattern(expected)})`, `[data-omp-role="parity.segment.${spec.id}"]`, expected,
      [{ entry: `segment:${spec.id}`, effect: "literal", value: expected }]),
    ...(spec.id === "token_rate" ? rateLiterals(expected)
      : spec.id === "time_spent" ? [] : [spanLiteral(spec.id, spanValue ?? expected)]),
    ...(spec.id === "path" ? [{
      name: "path does not gain native-only bold",
      terminal: { type: "screen" as const, pattern: `(${escapePattern(expected)})`, group: 1, read: "bold" as const },
      browser: { type: "dom" as const, selector: `[data-omp-role="parity.segment.path"] span:text-is(${JSON.stringify(expected)}):not(:has(span))`, read: "bold" as const },
      expected: false,
    }] : []),
    ...(spec.id === "pr" ? [linkComparison(
      "production PR hyperlink", expected,
      index ? "https://example.invalid/pull/42" : "https://github.com/can1357/oh-my-pi/pull/1842",
      `[data-omp-role="parity.segment.pr"] a:text-is(${JSON.stringify(expected)})`,
      { checkpoint: `production context ${index + 1}`, comparison: "exact production span" },
      "property:TspSpan.href",
    )] : []),
    ...(spec.id === "cache_hit" && index === 0 ? [{
      name: "icon leads the segment",
      terminal: { type: "screen" as const, pattern: `(\\S+)(?= ${escapePattern(expected)})`, group: 1, read: "leftHalf" as const },
      browser: { type: "dom" as const, selector: '[data-omp-role="parity.segment.cache_hit"] svg', read: "leftHalf" as const, relativeTo: '[id^="omp-view-"][data-omp-surface]' },
      expected: true,
    }] : []),
    callbackValue("context mutation applied", "segment-variant", index),
  ];
}

const segmentCases: FixtureCase[] = segmentSpecs.map(spec => ({
  id: `segment-${spec.id}`,
  covers: [
    `segment:${spec.id}`,
    `component:packages/tui/src/status-line/segments.ts#${segmentComponents[spec.id]}`,
    ...(spec.id === "token_rate" ? ["kind:rate", "property:TspRateProps.value", "property:TspRateProps.unit"]
      : spec.id === "time_spent" ? [] : ["property:TspSpan.t"]),
    ...(spec.id === "pr" ? ["property:TspSpan.href"] : []),
  ],
  async create(context) {
    const production = await context.load<typeof import("@omp-source/packages/tui/src/status-line/segments")>("packages/tui/src/status-line/segments.ts");
    const gallery = await context.load<typeof import("@omp-source/packages/coding-agent/src/cli/gallery-fixtures/segments")>("packages/coding-agent/src/cli/gallery-fixtures/segments.ts");
    const { node } = await context.load<typeof import("@omp-source/packages/tui/src/native/describe")>("packages/tui/src/native/describe.ts");
    assertRegistry(production.ALL_SEGMENT_IDS, segmentSpecs.map(candidate => candidate.id), "segment");
    assertRegistry(gallery.getSegmentGalleryInventory(), production.ALL_SEGMENT_IDS, "gallery segment");
    const entry = gallery.getSegmentGalleryEntries().find(candidate => candidate.id === spec.id);
    if (!entry?.variants.length || !production.SEGMENTS[spec.id]?.describe) throw new Error(`Missing production segment fixture ${spec.id}`);
    let restoreHyperlinks: (() => void) | undefined;
    if (spec.id === "pr") {
      const terminal = await context.load<typeof TerminalCapabilitiesModule>("packages/tui/src/terminal-capabilities.ts");
      const previous = terminal.TERMINAL.hyperlinks;
      terminal.setTerminalHyperlinks(true);
      restoreHyperlinks = () => terminal.setTerminalHyperlinks(previous);
    }
    const segmentContext = gallery.createGallerySegmentContext();
    segmentContext.options.pi = { showProfile: false };
    Object.assign(segmentContext.usageStats, { input: 12, output: 3, cacheRead: 48, cacheWrite: 1, orchestrationInput: 0, orchestrationOutput: 0, premiumRequests: 0 });
    const variants = resolveVariants(spec);
    let restoreProjectDirectory: (() => void) | undefined;
    let enterProjectDirectory: (() => void) | undefined;
    if (variants.some(variant => variant.projectDirectory)) {
      const dirs = await context.load<typeof import("@omp-source/packages/utils/src/dirs")>("packages/utils/src/dirs.ts");
      const previous = dirs.getProjectDir();
      await mkdir(scratchProjectDirectory, { recursive: true });
      enterProjectDirectory = () => dirs.setProjectDir(scratchProjectDirectory);
      restoreProjectDirectory = () => dirs.setProjectDir(previous);
    }
    let index = 0;
    let hidden = false;
    const selectVariant = () => {
      const variant = variants[index];
      if (variant.projectDirectory) enterProjectDirectory?.();
      variant.apply(segmentContext, index);
      const rendered = production.renderSegment(spec.id, segmentContext).visible;
      const described = production.describeSegment(spec.id, segmentContext) !== null;
      hidden = variant.kind === "hidden";
      if (hidden ? rendered || described : !rendered || !described) {
        throw new Error(`Segment ${spec.id} variant ${index} ${hidden ? "unexpectedly visible" : "unexpectedly hidden"}`);
      }
      context.record("segment-variant", index);
      context.record("segment-visibility", { rendered, described });
    };
    selectVariant();
    return controlledGallery(context, {
      render(width) {
        segmentContext.width = width;
        return [production.renderSegment(hidden ? sentinelId : spec.id, segmentContext).content];
      },
      describe() {
        const shown = hidden ? sentinelId : spec.id;
        const view = production.describeSegment(shown, segmentContext);
        if (!view) throw new Error(`Missing native segment ${shown}`);
        return node("status", { transparent: true, role: "parity.segment.bar" }, [node("seg", {
          role: `parity.segment.${shown}`, spans: view.spans, icon: view.icon, tone: view.tone, title: view.title,
        }, view.motion, shown)]);
      },
      invalidate() {},
      dispose() { restoreHyperlinks?.(); restoreProjectDirectory?.(); },
    }, {
      next() {
        if (++index >= variants.length) throw new Error(`No unexercised variant for ${spec.id}`);
        selectVariant();
      },
    });
  },
  checkpoints: resolveVariants(spec).map((variant, index) => ({
    name: `production context ${index + 1}`,
    ...(index ? { terminalInput: "n", browserActions: [{ type: "click" as const, selector: 'button:has-text("next")' }] } : {}),
    comparisons: variant.kind === "hidden" ? hiddenComparisons(spec, index)
      : variant.kind === "live" ? liveComparisons(spec, variant.pattern ?? "", index)
      : visibleComparisons(spec, variant.expected ?? "", variant.spanValue, index),
  })),
}));

export const fixtureCases: FixtureCase[] = [...segmentCases, ...statusLineCases];
