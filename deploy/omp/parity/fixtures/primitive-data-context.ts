import type * as ContextViewerModule from "@omp-source/packages/tui/src/overlays/context-viewer";
import type { ContextViewData } from "@omp-source/packages/tui/src/chrome/context-view-model";
import type { Comparison, FixtureCase } from "../scenario";

const meter = '[data-omp-role="omp.context.meter"], [role="meter"]';

function data(): ContextViewData {
  const entry = (id: string, label: string, tokens: number) => ({ id, label, text: `${label} fixture text`, tokens });
  return {
    breakdown: {
      model: undefined, contextWindow: 32_000, categories: [], thresholdTokens: 28_000,
      usedTokens: 9_000, freeTokens: 19_000, autoCompactBufferTokens: 4_000,
    },
    categories: [
      { id: "system-prompt", label: "System Prompt", tokens: 4_200, entries: [entry("sp", "Base prompt", 4_200)] },
      { id: "built-in-tools", label: "Built-in Tools", tokens: 2_100, entries: [entry("bt", "read", 2_100)] },
      { id: "user-messages", label: "User / Developer Messages", tokens: 900, entries: [entry("um", "First message", 900)] },
      { id: "tool-output", label: "Tool Output", tokens: 1_800, entries: [entry("to", "Fixture read output", 1_800)] },
    ],
  };
}

const probe = (name: string): Comparison => ({
  name,
  terminal: { type: "screen", pattern: "(Free Space)", group: 1, read: "visible" },
  browser: { type: "dom", selector: meter, read: "visible" },
  expected: true,
});

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-context-meter-overview",
    covers: ["kind:meter", "component:packages/tui/src/overlays/context-viewer.ts#ContextViewer"],
    async create(context) {
      const { ContextViewer } = await context.load<typeof ContextViewerModule>("packages/tui/src/overlays/context-viewer.ts");
      return new ContextViewer(data(), { onClose: () => context.done(), requestRender: () => context.tui.requestRender(), getRows: () => 40 });
    },
    checkpoints: [{ name: "context overview", comparisons: [probe("meter present")] }],
  },
];
