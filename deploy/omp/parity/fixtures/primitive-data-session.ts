import type * as TextModule from "@omp-source/packages/tui/src/components/text";
import type { Comparison, FixtureCase } from "../scenario";

const effort = '[data-omp-role="omp.composer.model.effort"]';
const modelName = '[data-omp-node$="/model/name"]';
const contextMeter = '[data-omp-role="omp.composer.context"]';

/** The real session composer's chip row: the thinking level glyph leads the model name on both sides. */
const effortOnModelRow: Comparison = {
  name: "thinking level glyph leads the model name",
  terminal: { type: "screen", pattern: "◒", relativeTo: { pattern: "parity-coder" }, read: "sameRow" },
  browser: { type: "dom", selector: effort, relativeTo: modelName, read: "sameRow" },
  expected: true,
  proves: [{ entry: "property:TspEffortProps.level", effect: "layout", value: "high" }],
};

const contextWindow: Comparison = {
  name: "context window size",
  terminal: { type: "screen", pattern: "(33K)", group: 1 },
  browser: { type: "dom", selector: contextMeter, read: "text", pattern: "/ (33K)", group: 1 },
  expected: "33K",
  normalize: "whitespace",
  proves: [{ entry: "property:TspMeterProps.total", effect: "literal", value: "33K" }],
};

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-composer-effort-context",
    covers: ["kind:effort", "kind:meter", "property:TspEffortProps.level", "property:TspMeterProps.total"],
    async create(context) {
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      return new Text("Composer chips are drawn by the real session.", 0, 0);
    },
    checkpoints: [{ name: "composer chips", comparisons: [effortOnModelRow, contextWindow] }],
  },
];
