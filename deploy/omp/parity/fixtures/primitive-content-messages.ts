import type * as Ui from "@omp-source/packages/tui/src/index";
import type * as Fireworks from "@omp-source/packages/tui/src/overlays/codex-reset-fireworks";
import type * as UserMessage from "@omp-source/packages/tui/src/chat/user-message";
import type { Comparison, FixtureCase } from "../scenario";
import { contentComparison } from "./gallery-support";

const celebrationTop = (name: string, pattern: string, selector: string, proves: Comparison["proves"]): Comparison => ({
  name,
  terminal: { type: "screen", pattern, read: "topHalf" },
  browser: { type: "dom", selector, read: "topHalf" },
  expected: true,
  proves,
});

const chipMark = 'span:text-is("[Paste #1, 3 lines]")';
const chipParagraph = `p:has(> ${chipMark})`;

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-user-message-paste-chip",
    covers: ["component:packages/tui/src/chat/user-message.ts#UserMessageComponent", "property:TspMarkdownProps.marks"],
    async create(context) {
      const { UserMessageComponent } = await context.load<typeof UserMessage>("packages/tui/src/chat/user-message.ts");
      return new UserMessageComponent("Review [Paste #1, 3 lines] before continuing");
    },
    checkpoints: [{ name: "user message with a paste chip", comparisons: [
      contentComparison("message text", "(Review .* before continuing)", chipParagraph, "Review [Paste #1, 3 lines] before continuing"),
      { name: "paste chip is marked inside the message", terminal: { type: "screen", pattern: "\\[Paste #1, 3 lines\\]", read: "sameRow", relativeTo: { pattern: "Review" } }, browser: { type: "dom", selector: chipMark, read: "sameRow", relativeTo: chipParagraph }, expected: true, proves: [
        { entry: "property:TspMarkdownProps.marks", effect: "layout", value: [{ t: "[Paste #1, 3 lines]", s: "accent strong" }] },
      ] },
    ] }],
  },
  {
    id: "primitive-fireworks-shimmer-harvest",
    covers: [
      "component:packages/tui/src/overlays/codex-reset-fireworks.ts#CodexResetFireworksComponent",
      "property:TspColProps.align", "property:TspColProps.gap", "property:TspSpan.fx", "property:TspShimmerProps.text",
      "property:TspShimmerProps.palette.low", "property:TspShimmerProps.palette.mid", "property:TspShimmerProps.palette.high",
    ],
    async create(context) {
      const { Container } = await context.load<typeof Ui>("packages/tui/src/index.ts");
      const { CodexResetFireworksController } = await context.load<typeof Fireworks>("packages/tui/src/overlays/codex-reset-fireworks.ts");
      const owner = new CodexResetFireworksController({ ui: {
        terminal: context.tui.terminal, requestRender: () => context.tui.requestRender(), setFocus: component => context.tui.setFocus(component),
        showOverlay: (component, options) => context.tui.showOverlay(component, options),
      } });
      setTimeout(() => { owner.show({ kind: "saved-reset-banked", added: 1, available: 2 }); }, 0);
      return Object.assign(new Container(), { dispose: () => owner.dispose() });
    },
    checkpoints: [{ name: "banked reset celebration", comparisons: [
      contentComparison("saved reset information", "(New reset banked · 2 available)", '[data-omp-role="omp.overlay.fireworks"]', "New reset banked · 2 available"),
      contentComparison("shimmer title", "(Saved reset)", '[data-omp-node="m.1"]', "Saved reset", [{ entry: "property:TspShimmerProps.text", effect: "literal", value: "Saved reset" }]),
      celebrationTop("celebration column", "Saved reset", '[data-omp-node="m"]', [
        { entry: "property:TspColProps.align", effect: "layout", value: "center" },
        { entry: "property:TspColProps.gap", effect: "layout", value: "sm" },
      ]),
      celebrationTop("shimmer palette", "Saved reset", '[data-omp-node="m.1"]', [
        { entry: "property:TspShimmerProps.palette.low", effect: "layout", value: "thinkingXhigh" },
        { entry: "property:TspShimmerProps.palette.mid", effect: "layout", value: "warning" },
        { entry: "property:TspShimmerProps.palette.high", effect: "layout", value: "text" },
      ]),
      celebrationTop("pulsing sparks", "✦ ✧ ✦", '[data-omp-node="m.0"]', [{ entry: "property:TspSpan.fx", effect: "layout", value: "pulse" }]),
    ] }],
  },
];
