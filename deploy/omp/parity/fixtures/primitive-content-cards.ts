import type * as ToolCardModule from "@omp-source/packages/tui/src/render/tool-card";
import type * as MessageFrameModule from "@omp-source/packages/tui/src/chrome/message-frame";
import type * as DisclosureModule from "@omp-source/packages/tui/src/components/disclosure";
import type * as BoxModule from "@omp-source/packages/tui/src/components/box";
import type * as TextModule from "@omp-source/packages/tui/src/components/text";
import type * as Describe from "@omp-source/packages/tui/src/native/describe";
import type { Component } from "@omp-source/packages/tui/src/tui";
import type { Comparison, FixtureCase, FixtureContext } from "../scenario";
import { contentComparison, controlledGallery } from "./gallery-support";

const component = (file: string, name: string) => `component:packages/tui/src/${file}.ts#${name}`;

export async function mountComponents(context: FixtureContext, components: readonly Component[], actions: Readonly<Record<string, () => void>> = {}) {
  const { col } = await context.load<typeof Describe>("packages/tui/src/native/describe.ts");
  return controlledGallery(context, {
    render: (width: number) => components.flatMap(child => [...child.render(width)]),
    invalidate() { for (const child of components) child.invalidate?.(); },
    describe: () => col(components as never),
  } as never, actions);
}

const headRowOf = (name: string, terminalHead: string, headWord: string, selector: string, relativeTo: string, proves?: Comparison["proves"]): Comparison => ({
  name,
  terminal: { type: "screen", pattern: terminalHead, read: "sameRow", relativeTo: { pattern: headWord } },
  browser: { type: "dom", selector, read: "sameRow", relativeTo },
  expected: true,
  proves,
});
const headRow = (name: string, terminalHead: string, headWord: string, cardSelector: string, proves?: Comparison["proves"]): Comparison =>
  headRowOf(name, terminalHead, headWord, `${cardSelector} > div > p`, `${cardSelector} > div > p > span`, proves);
const cardHead = (title: string, nodeId: string, status: string): Comparison =>
  headRow(`${title} head row`, title, title.split(" ")[1]!, `[data-omp-node="${nodeId}"]`, [{ entry: "property:TspCardProps.status", effect: "layout", value: status }]);

const frameHead = (name: string, proves: Comparison["proves"]): Comparison => headRow(name, "frame-tag", "frame", '[data-omp-role="omp.frame-tag"]', proves);

const framedBody = ["Frame line one", "Frame line two", "Frame line three", "Frame line four", "Frame line five"].join("\n");

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-card-status-states",
    covers: [component("render/tool-card", "ToolCard"), "property:TspCardProps.status", "property:TspCardProps.inset", "property:TspImageProps.builtin"],
    async create(context) {
      const { framedToolCard, plainToolCard } = await context.load<typeof ToolCardModule>("packages/tui/src/render/tool-card.ts");
      const framed = (phase: "pending" | "running" | "success" | "error", icon: "pending" | "running" | "success" | "error") =>
        framedToolCard(context.theme, () => ({ status: { icon, title: `Card ${phase}` }, phase, body: [`Body of ${phase} card`] }));
      return mountComponents(context, [
        framed("pending", "pending"), framed("running", "running"), framed("success", "success"), framed("error", "error"),
        plainToolCard(context.theme, () => ({ status: { icon: "success", title: "Card plain" }, phase: "success", body: ["Body of plain card"] })),
      ]);
    },
    checkpoints: [{ name: "tool card status states", comparisons: [
      contentComparison("pending card body", "(Body of pending card)", '[data-omp-node="n"]', "Body of pending card"),
      contentComparison("running card body", "(Body of running card)", '[data-omp-node="o"]', "Body of running card"),
      contentComparison("plain card body", "(Body of plain card)", '[data-omp-node="r"]', "Body of plain card"),
      {
        name: "welcome mark beside its wordmark",
        terminal: { type: "screen", pattern: "▄▀▀▄ █▀▄▀▄", read: "sameRow", relativeTo: { pattern: "██  ██" } },
        browser: { type: "dom", selector: '[data-omp-node="1.lockup/logo"]', read: "sameRow", relativeTo: '[data-omp-node="1.lockup/mark"]' },
        expected: true,
        proves: [{ entry: "property:TspImageProps.builtin", effect: "layout", value: "omp" }],
      },
      cardHead("Card pending", "n", "pending"),
      cardHead("Card running", "o", "running"),
      cardHead("Card success", "p", "done"),
      cardHead("Card error", "q", "error"),
      headRow("Card plain head row", "Card plain", "plain", '[data-omp-node="r"]', [{ entry: "property:TspCardProps.inset", effect: "layout", value: true }]),
    ] }],
  },
  {
    id: "primitive-card-framed-message-collapse",
    covers: [component("chrome/message-frame", "FramedMessageComponent"), "property:TspCardProps.collapsed", "property:TspCardProps.collapsible", "property:TspCardProps.preview", "property:TspCardProps.head"],
    async create(context) {
      const { FramedMessageComponent } = await context.load<typeof MessageFrameModule>("packages/tui/src/chrome/message-frame.ts");
      return mountComponents(context, [new FramedMessageComponent({ message: { customType: "frame-tag", content: framedBody }, role: "omp.frame-tag", collapseAfterLines: 2 })]);
    },
    checkpoints: [
      { name: "collapsed framed card", comparisons: [
        contentComparison("frame head", "(frame-tag)", '[data-omp-role="omp.frame-tag"]', "frame-tag"),
        frameHead("collapsed frame card", [
          { entry: "property:TspCardProps.collapsed", effect: "layout", value: true },
          { entry: "property:TspCardProps.collapsible", effect: "layout", value: true },
          { entry: "property:TspCardProps.preview", effect: "layout", value: { lines: 2 } },
          { entry: "property:TspCardProps.head", effect: "layout", value: [{ t: "frame-tag", s: "customMessageLabel strong" }] },
        ]),
      ] },
      { name: "expanded framed card", browserActions: [{ type: "click", selector: '[data-omp-role="omp.frame-tag"] button' }], comparisons: [
        contentComparison("frame head after expand", "(frame-tag)", '[data-omp-role="omp.frame-tag"]', "frame-tag"),
        frameHead("expanded frame card", [{ entry: "property:TspCardProps.collapsed", effect: "layout", value: false }]),
      ] },
    ],
  },
  {
    id: "primitive-card-selected-box-and-section",
    covers: [component("components/box", "Box"), component("components/disclosure", "Disclosure"), "property:TspCardProps.selected", "property:TspSectionProps.collapsible", "property:TspSectionProps.collapsed"],
    async create(context) {
      const { Box } = await context.load<typeof BoxModule>("packages/tui/src/components/box.ts");
      const { Disclosure } = await context.load<typeof DisclosureModule>("packages/tui/src/components/disclosure.ts");
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      const selected = new Box(1, 0, text => context.theme.bg("selectedBg", text));
      selected.addChild(new Text("Selected panel content", 0, 0));
      const collapsed = new Disclosure({ summary: new Text("Collapsed section summary", 0, 0), body: () => new Text("Collapsed section body", 0, 0) });
      const expanded = new Disclosure({ summary: new Text("Expanded section summary", 0, 0), body: () => new Text("Expanded section body", 0, 0), expanded: true });
      return mountComponents(context, [selected, collapsed, expanded]);
    },
    checkpoints: [{ name: "selected panel and sections", comparisons: [
      contentComparison("selected panel", "(Selected panel content)", '[data-omp-node="q"]', "Selected panel content"),
      contentComparison("collapsed summary", "(Collapsed section summary)", '[data-omp-node="r"]', "Collapsed section summary"),
      contentComparison("expanded body", "(Expanded section body)", '[data-omp-node="s"]', "Expanded section body"),
      headRowOf("selected panel wraps its content", "Selected panel content", "Selected", '[data-omp-node="n"]', '[data-omp-node="q"]', [
        { entry: "property:TspCardProps.selected", effect: "layout", value: true },
      ]),
      headRowOf("collapsed section toggle", "Collapsed section summary", "Collapsed", '[data-omp-node="o"] [data-name="CollapsibleTrigger"]', '[data-omp-node="r"]', [
        { entry: "property:TspSectionProps.collapsible", effect: "layout", value: true },
        { entry: "property:TspSectionProps.collapsed", effect: "layout", value: true },
      ]),
      headRowOf("expanded section toggle", "Expanded section summary", "Expanded", '[data-omp-node="p"] [data-name="CollapsibleTrigger"]', '[data-omp-node="t"]', [
        { entry: "property:TspSectionProps.collapsed", effect: "layout", value: false },
      ]),
    ] }],
  },
];
