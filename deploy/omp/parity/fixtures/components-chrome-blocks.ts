import type * as DynamicBorderModule from "@omp-source/packages/tui/src/chrome/dynamic-border";
import type * as StreamingPanelModule from "@omp-source/packages/tui/src/chrome/streaming-panel";
import type * as ToolActivityModule from "@omp-source/packages/tui/src/chrome/tool-activity";
import type * as TranscriptNoticeModule from "@omp-source/packages/tui/src/chrome/transcript-notice";
import type * as TranscriptStatusModule from "@omp-source/packages/tui/src/chrome/transcript-status";
import type * as DescribeModule from "@omp-source/packages/tui/src/native/describe";
import type * as TextModule from "@omp-source/packages/tui/src/components/text";
import type * as OverlayBoxModule from "@omp-source/packages/tui/src/chrome/overlay-box";
import type * as OutputPaneModule from "@omp-source/packages/tui/src/render/output-pane";
import type * as TuiModule from "@omp-source/packages/tui/src/tui";
import type { FixtureCase } from "../scenario";
import { componentId, shown } from "./components-support";

const chrome = (file: string, name: string) => componentId(`chrome/${file}`, name);

export const fixtureCases: FixtureCase[] = [
  {
    id: "comp-chrome-transcript-blocks",
    covers: [
      chrome("dynamic-border", "DynamicBorder"),
      chrome("transcript-notice", "TranscriptNotice"),
      chrome("transcript-status", "TranscriptStatusBlock"),
      chrome("tool-activity", "ToolActivityContainer"),
      chrome("streaming-panel", "StreamingPanelContent"),
      chrome("streaming-panel", "StreamingPanelFooter"),
    ],
    async create(context) {
      const { Container } = await context.load<typeof TuiModule>("packages/tui/src/tui.ts");
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      const { span } = await context.load<typeof DescribeModule>("packages/tui/src/native/describe.ts");
      const { DynamicBorder } = await context.load<typeof DynamicBorderModule>("packages/tui/src/chrome/dynamic-border.ts");
      const { TranscriptNotice } = await context.load<typeof TranscriptNoticeModule>("packages/tui/src/chrome/transcript-notice.ts");
      const { TranscriptStatusBlock } = await context.load<typeof TranscriptStatusModule>("packages/tui/src/chrome/transcript-status.ts");
      const { ToolActivityContainer } = await context.load<typeof ToolActivityModule>("packages/tui/src/chrome/tool-activity.ts");
      const { StreamingPanelContent } = await context.load<typeof StreamingPanelModule>("packages/tui/src/chrome/streaming-panel.ts");
      const root = new Container();
      root.addChild(new DynamicBorder());
      root.addChild(TranscriptNotice.line("warning", "Notice fixture warning line"));
      root.addChild(new TranscriptStatusBlock([{
        parts: [{ text: "Status fixture row" }],
        native: { tone: "info", message: [span("Status fixture row")] },
      }]));
      root.addChild(new ToolActivityContainer(new Text("Activity fixture row", 0, 0)));
      root.addChild(new StreamingPanelContent(() => ({
        sections: [new Text("Panel section fixture row", 0, 0)],
        footer: "Panel footer fixture row",
      })));
      return root;
    },
    checkpoints: [{
      name: "chrome blocks",
      comparisons: [
        shown("warning notice", "Notice fixture warning line"),
        shown("status row", "Status fixture row"),
        shown("activity row", "Activity fixture row"),
        shown("panel section", "Panel section fixture row"),
        shown("panel footer", "Panel footer fixture row"),
      ],
    }],
  },
  {
    id: "comp-chrome-output-pane-divider",
    covers: [chrome("overlay-box", "PanelDivider"), componentId("render/output-pane", "OutputPane")],
    async create(context) {
      const { Container } = await context.load<typeof TuiModule>("packages/tui/src/tui.ts");
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      const { PanelDivider } = await context.load<typeof OverlayBoxModule>("packages/tui/src/chrome/overlay-box.ts");
      const { OutputPane } = await context.load<typeof OutputPaneModule>("packages/tui/src/render/output-pane.ts");
      const container = new Container();
      container.addChild(new Text("Above divider fixture row", 0, 0));
      container.addChild(new PanelDivider());
      container.addChild(new OutputPane(context.theme, { expanded: true, collapsedMaxLines: 5, paddingX: 0 }, "Output pane row one\nOutput pane row two"));
      return container;
    },
    checkpoints: [{
      name: "divider and output rows",
      comparisons: [shown("above divider", "Above divider fixture row"), shown("output row", "Output pane row two")],
    }],
  },
];
