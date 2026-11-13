import type * as TextModule from "@omp-source/packages/tui/src/components/text";
import type * as ToolCardModule from "@omp-source/packages/tui/src/render/tool-card";
import type { FixtureCase } from "../scenario";
import { componentId, shown } from "./components-support";

const toolCard = componentId("render/tool-card", "ToolCard");
const cachedOutputBlock = componentId("render/output-block", "CachedOutputBlock");
const tool = "packages/tui/src/render/tool-card.ts";

export const fixtureCases: FixtureCase[] = [
  {
    id: "comp-render-tool-card-framed-status",
    covers: [toolCard, cachedOutputBlock],
    async create(context) {
      const { framedToolCard } = await context.load<typeof ToolCardModule>(tool);
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      const detail = new Text("Fixture card component section", 0, 0);
      return framedToolCard(context.theme, () => ({
        status: { icon: "success", title: "Fixture card title", description: "fixture card description", meta: ["fixture meta"] },
        phase: "success",
        headerMeta: "fixture header meta",
        body: ["Fixture card body row one", "Fixture card body row two"],
        sections: [{ label: "Fixture section label", content: detail, separator: true }],
        footer: ["Fixture card footer row"],
        footerMeta: "fixture footer meta",
      }));
    },
    checkpoints: [
      { name: "framed card", comparisons: [shown("title", "Fixture card title"), shown("body", "Fixture card body row two"), shown("section", "Fixture card component section"), shown("footer", "Fixture card footer row")] },
      { name: "input forwarded to card", terminalInput: "x", comparisons: [shown("body stays", "Fixture card body row one")] },
    ],
  },
  {
    id: "comp-render-tool-card-framed-header",
    covers: [toolCard, cachedOutputBlock],
    async create(context) {
      const { framedToolCard } = await context.load<typeof ToolCardModule>(tool);
      return framedToolCard(context.theme, () => ({
        header: "Fixture header string",
        phase: "error",
        body: ["Fixture error body row"],
        borderColor: "error",
      }));
    },
    checkpoints: [
      { name: "framed header card", comparisons: [shown("header", "Fixture header string"), shown("body", "Fixture error body row")] },
    ],
  },
  {
    id: "comp-render-tool-card-plain",
    covers: [toolCard],
    async create(context) {
      const { plainToolCard } = await context.load<typeof ToolCardModule>(tool);
      return plainToolCard(context.theme, () => ({
        header: "Fixture plain header",
        phase: "success",
        body: ["Fixture plain body row"],
        footerMeta: "fixture plain footer",
      }), { ignoreTight: false, paddingX: 1 });
    },
    checkpoints: [
      { name: "plain card", comparisons: [shown("header", "Fixture plain header"), shown("body", "Fixture plain body row"), shown("footer", "fixture plain footer")] },
    ],
  },
];
