import type * as SplitPaneModule from "@omp-source/packages/tui/src/components/layout/split-pane";
import type * as StackModule from "@omp-source/packages/tui/src/components/layout/stack";
import type * as ScrollViewModule from "@omp-source/packages/tui/src/components/scroll-view";
import type * as TruncatedTextModule from "@omp-source/packages/tui/src/components/truncated-text";
import type * as WidthAwareTextModule from "@omp-source/packages/tui/src/render/width-aware-text";
import type * as TextModule from "@omp-source/packages/tui/src/components/text";
import type * as DisclosureModule from "@omp-source/packages/tui/src/components/disclosure";
import type { FixtureCase } from "../scenario";
import { componentId, shown } from "./components-support";

const components = (file: string, name: string) => componentId(`components/${file}`, name);

export const fixtureCases: FixtureCase[] = [
  {
    id: "comp-chrome-layout-primitives",
    covers: [
      components("layout/split-pane", "SplitPane"),
      components("layout/stack", "Stack"),
      components("scroll-view", "ScrollView"),
      components("truncated-text", "TruncatedText"),
      componentId("render/width-aware-text", "WidthAwareText"),
    ],
    async create(context) {
      const { SplitPane } = await context.load<typeof SplitPaneModule>("packages/tui/src/components/layout/split-pane.ts");
      const { Stack } = await context.load<typeof StackModule>("packages/tui/src/components/layout/stack.ts");
      const { ScrollView } = await context.load<typeof ScrollViewModule>("packages/tui/src/components/scroll-view.ts");
      const { TruncatedText } = await context.load<typeof TruncatedTextModule>("packages/tui/src/components/truncated-text.ts");
      const { WidthAwareText } = await context.load<typeof WidthAwareTextModule>("packages/tui/src/render/width-aware-text.ts");
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      const left = new Stack({
        children: [
          { content: new TruncatedText("Left truncated fixture row", 0, 0), height: 1 },
          { content: new WidthAwareText(width => `Left width aware fixture row ${width > 0 ? "sized" : ""}`, 0, 0), height: 1 },
        ],
      });
      const right = new ScrollView(new Text("Right scroll fixture row one\nRight scroll fixture row two", 0, 0), { height: 4 });
      return new SplitPane({ left, right, leftSize: { fixed: 36 }, height: 4 });
    },
    checkpoints: [{
      name: "split pane layout",
      comparisons: [
        shown("truncated text", "Left truncated fixture row"),
        shown("width aware text", "Left width aware fixture row sized"),
        shown("scroll view row", "Right scroll fixture row one"),
      ],
    }],
  },
  {
    id: "comp-chrome-disclosure-expand",
    covers: [components("disclosure", "Disclosure")],
    async create(context) {
      const { Disclosure } = await context.load<typeof DisclosureModule>("packages/tui/src/components/disclosure.ts");
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      return new Disclosure({ summary: new Text("Disclosure fixture summary", 0, 0), body: () => new Text("Disclosure fixture body", 0, 0) });
    },
    checkpoints: [
      { name: "collapsed disclosure", comparisons: [shown("summary", "Disclosure fixture summary")] },
      { name: "expand disclosure", browserActions: [{ type: "click", selector: '[data-name="Collapsible"] button' }], comparisons: [shown("summary after expand", "Disclosure fixture summary")] },
    ],
  },
  {
    id: "comp-chrome-layout-ratio-scroll-lines",
    covers: [
      components("layout/split-pane", "SplitPane"),
      components("layout/stack", "Stack"),
      components("scroll-view", "ScrollView"),
    ],
    async create(context) {
      const { SplitPane } = await context.load<typeof SplitPaneModule>("packages/tui/src/components/layout/split-pane.ts");
      const { Stack } = await context.load<typeof StackModule>("packages/tui/src/components/layout/stack.ts");
      const { ScrollView } = await context.load<typeof ScrollViewModule>("packages/tui/src/components/scroll-view.ts");
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      const left = new Stack({ children: [{ content: new Text("Auto height stack row", 0, 0) }] });
      const right = new ScrollView(["Lines scroll row one", "Lines scroll row two", "Lines scroll row three", "Lines scroll row four", "Lines scroll row five", "Lines scroll row six"], { height: 3, scrollbar: true });
      return new SplitPane({ left, right, leftSize: { ratio: 0.4 }, height: 3 });
    },
    checkpoints: [{
      name: "ratio split with auto height and scrolled lines",
      comparisons: [shown("auto height row", "Auto height stack row"), shown("scroll lines row", "Lines scroll row one")],
    }],
  },
];
