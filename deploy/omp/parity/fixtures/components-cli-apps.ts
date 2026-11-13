import type * as ComposerGallery from "@omp-source/packages/coding-agent/src/cli/gallery-fixtures/composer";
import type * as FsGallery from "@omp-source/packages/coding-agent/src/cli/gallery-fixtures/fs";
import type * as SegmentGallery from "@omp-source/packages/coding-agent/src/cli/gallery-fixtures/segments";
import type * as StatusLineGallery from "@omp-source/packages/coding-agent/src/cli/gallery-fixtures/status-line";
import type * as BoxModule from "@omp-source/packages/tui/src/components/box";
import type * as TextModule from "@omp-source/packages/tui/src/components/text";
import type { FixtureCase, FixtureContext } from "../scenario";
import { contentComparison } from "./gallery-support";

const component = (file: string, name: string) => `component:packages/coding-agent/src/cli/gallery-fixtures/${file}.ts#${name}`;
const WIDTH = 100;

/** Mounts the gallery's production-rendered rows, one text row per line, so both legs show the same producer output. */
async function mountRows(context: FixtureContext, rows: readonly string[]) {
  const { Box } = await context.load<typeof BoxModule>("packages/tui/src/components/box.ts");
  const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
  const root = new Box(0, 0);
  for (const row of rows) root.addChild(new Text(row, 0, 0));
  return root;
}

const rowComparison = (name: string, phrase: string, expected = phrase) =>
  contentComparison(name, `(${phrase.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`, `text=/${phrase.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&")}/ >> nth=0`, expected);


export const fixtureCases: FixtureCase[] = [
  {
    id: "gallery-composer-canonical-preview",
    covers: [component("composer", "getComposerGalleryEntries.variants"), "feature:scroll"],
    async create(context) {
      const gallery = await context.load<typeof ComposerGallery>("packages/coding-agent/src/cli/gallery-fixtures/composer.ts");
      const entry = gallery.getComposerGalleryEntries().find(candidate => candidate.id === "box");
      if (!entry?.variants.length) throw new Error("Missing production composer gallery entry box");
      return mountRows(context, await entry.variants[0]!.render(WIDTH, false));
    },
    checkpoints: [{ name: "gallery composer preview rows", comparisons: [rowComparison("canonical preview prompt", "Ask anything, edit files, run tools")] }],
  },
  {
    id: "gallery-fs-read-group-success",
    covers: [component("fs", "fsFixtures.read_group")],
    async create(context) {
      const gallery = await context.load<typeof FsGallery>("packages/coding-agent/src/cli/gallery-fixtures/fs.ts");
      const fixture = gallery.fsFixtures.read_group;
      if (!fixture?.renderState) throw new Error("Missing production read_group gallery fixture");
      return mountRows(context, await fixture.renderState("success", WIDTH, true));
    },
    checkpoints: [{ name: "gallery read group rows", comparisons: [rowComparison("grouped read target", "streaming-preview-height.test.ts")] }],
  },
  {
    id: "gallery-segment-model-variant",
    covers: [component("segments", "getSegmentGalleryEntries.variants")],
    async create(context) {
      const gallery = await context.load<typeof SegmentGallery>("packages/coding-agent/src/cli/gallery-fixtures/segments.ts");
      const entry = gallery.getSegmentGalleryEntries().find(candidate => candidate.id === "model");
      if (!entry?.variants.length) throw new Error("Missing production segment gallery entry model");
      return mountRows(context, await entry.variants[0]!.render(WIDTH, false));
    },
    checkpoints: [{ name: "gallery segment rows", comparisons: [rowComparison("model segment", "Sonnet 4.5")] }],
  },
  {
    id: "gallery-status-line-context-gauge",
    covers: [component("status-line", "statusLineFixtures.context_gauge")],
    async create(context) {
      const gallery = await context.load<typeof StatusLineGallery>("packages/coding-agent/src/cli/gallery-fixtures/status-line.ts");
      const fixture = gallery.statusLineFixtures.context_gauge;
      if (!fixture?.renderState) throw new Error("Missing production context_gauge gallery fixture");
      return mountRows(context, await fixture.renderState("success", WIDTH, false));
    },
    checkpoints: [{ name: "gallery gauge rows", comparisons: [rowComparison("gauge usage note", "97% used — past compaction threshold")] }],
  },
];
