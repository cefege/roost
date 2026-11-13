import type * as BashExecution from "@omp-source/packages/tui/src/chat/bash-execution";
import type * as LoaderModule from "@omp-source/packages/tui/src/components/loader";
import type { Comparison, FixtureCase } from "../scenario";
import { contentComparison } from "./gallery-support";

const outputLines = Array.from({ length: 30 }, (_, index) => `bash-line-${String(index + 1).padStart(2, "0")}`).join("\n");
const bash = '[data-omp-role="omp.bash"]';
const stop = '[data-omp-role="omp.working.stop"]';

const bashPreview: FixtureCase = {
  id: "primitive-preview-lines-bash-tail",
  covers: ["property:TspPreview.lines", "component:packages/tui/src/chat/bash-execution.ts#BashExecutionComponent"],
  async create(context) {
    const { BashExecutionComponent } = await context.load<typeof BashExecution>("packages/tui/src/chat/bash-execution.ts");
    const component = new BashExecutionComponent("echo bash-fixture", context.tui);
    component.setComplete(0, false, { output: outputLines });
    return component;
  },
  checkpoints: [{ name: "collapsed bash tail", comparisons: [
    contentComparison("last previewed line", "(bash-line-30)", `${bash} :is(span,div,pre):not(:has(*)):has-text("bash-line-30")`, "bash-line-30"),
    { name: "preview spans twenty rows", terminal: { type: "screen", pattern: "bash-line-30", relativeTo: { pattern: "bash-line-11" }, read: "rowGap" }, browser: { type: "dom", selector: `${bash} :is(span,div,pre):not(:has(*)):has-text("bash-line-30")`, relativeTo: `${bash} :is(span,div,pre):not(:has(*)):has-text("bash-line-11")`, read: "rowGap" }, expected: 19,
      proves: [{ entry: "property:TspPreview.lines", effect: "layout", value: 20, target: { checkpoint: "collapsed bash tail", comparison: "preview spans twenty rows" } }] },
    { name: "line before the preview is clamped", terminal: { type: "screen", pattern: "bash-line-10", read: "visible" }, browser: { type: "dom", selector: `${bash} :is(span,div,pre):not(:has(*)):has-text("bash-line-10")`, read: "visible" }, expected: false },
  ] }],
};

const workingKey: FixtureCase = {
  id: "primitive-kbd-working-row-key",
  covers: ["kind:kbd", "property:TspKbdProps.keys", "component:packages/tui/src/components/loader.ts#Loader"],
  async create(context) {
    const { Loader } = await context.load<typeof LoaderModule>("packages/tui/src/components/loader.ts");
    const loader = new Loader(context.tui, text => context.theme.fg("accent", text), text => context.theme.fg("muted", text), "Indexing fixture sources");
    loader.setWorkingRow(() => ({ label: "Indexing fixture sources", startedAt: Date.now(), interruptKey: "q" }), () => undefined);
    loader.setTrailer?.(() => "q to interrupt");
    return loader;
  },
  checkpoints: [{ name: "interrupt keycap", comparisons: [{
    name: "interrupt key",
    terminal: { type: "screen", pattern: "(q) to interrupt", group: 1 },
    browser: { type: "dom", selector: `${stop} kbd:not(:has(kbd))`, read: "text" },
    expected: "q",
    proves: [{ entry: "property:TspKbdProps.keys", effect: "content", value: ["q"], parts: [{ pointer: "/0", effect: "literal", comparison: { checkpoint: "interrupt keycap", comparison: "interrupt key" } }] }],
  } satisfies Comparison] }],
};

export const fixtureCases: FixtureCase[] = [bashPreview, workingKey];
