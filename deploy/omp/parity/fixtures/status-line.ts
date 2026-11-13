import type * as StatusLineComponentModule from "@omp-source/packages/tui/src/status-line/component";
import type * as PreviewSessionModule from "@omp-source/packages/coding-agent/src/cli/gallery-fixtures/preview-session";
import type * as SegmentsModule from "@omp-source/packages/tui/src/status-line/segments";
import type * as SegmentGalleryModule from "@omp-source/packages/coding-agent/src/cli/gallery-fixtures/segments";
import type * as DescribeModule from "@omp-source/packages/tui/src/native/describe";
import type * as DirsModule from "@omp-source/packages/utils/src/dirs";
import type * as FooterModule from "@omp-source/packages/tui/src/status-line/footer";
import type * as StatusLineHostModule from "@omp-source/packages/coding-agent/src/modes/status-line-host";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import type { Comparison, FixtureCase, FixtureContext } from "../scenario";
import { callbackValue } from "../scenario";
import { controlledGallery } from "./gallery-support";
import { createSettings } from "./settings-support";

// Production footer truncation branches only run when the width cannot hold both stat groups.
const narrowFooterWidths = [8, 24, 40, 56, 72];
const pathSegment = '[data-omp-role="omp.status.path"]';
const modelSegment = '[data-omp-role="omp.status.model"]';
const costSegment = '[data-omp-role="omp.status.cost"]';
const statusBar = '[data-omp-role="omp.status"] [data-omp-status="true"]';
const barPath = "Sonnet 4\\.5.*?(workspace)";

const barComparison = (name: string, terminalPattern: string, selector: string, expected: string): Comparison => ({
  name,
  terminal: { type: "screen", pattern: terminalPattern, group: 1 },
  browser: { type: "dom", selector, read: "text", pattern: terminalPattern, group: 1 },
  expected,
  normalize: "whitespace",
});

async function mountStatusLine(context: FixtureContext, transparent: boolean, hookStatus: boolean, band = false) {
  const { StatusLineComponent } = await context.load<typeof StatusLineComponentModule>("packages/tui/src/status-line/component.ts");
  const { createGallerySession } = await context.load<typeof PreviewSessionModule>("packages/coding-agent/src/cli/gallery-fixtures/preview-session.ts");
  const { statusLineHost } = await context.load<typeof StatusLineHostModule>("packages/coding-agent/src/modes/status-line-host.ts");
  const status = new StatusLineComponent(createGallerySession(), statusLineHost);
  status.updateSettings({
    preset: "custom", leftSegments: ["model", "path"], rightSegments: ["cost"],
    separator: "powerline-thin", sessionAccent: false, transparent, contextLine: "annotated",
  });
  if (hookStatus) status.setHookStatus("parity", "Parity hook status");
  status.onNativeAction = action => context.record("status-action", action);
  return controlledGallery(context, {
    render(width) {
      const preview = band ? { statusAttachment: "top-band" as const, bottomBar: "none" as const } : { statusAttachment: "none" as const, bottomBar: "full" as const };
      return [...status.getPreviewLines(width, preview), ...status.render(width)];
    },
    describe() { return status.describePreview(); },
    handleInput(data) {
      if (data === "p") status.handleNativeEvent({ type: "action", key: "", act: "status.path", mods: [] });
    },
    handleNativeEvent(event) { status.handleNativeEvent(event); },
    invalidate() { status.invalidate(); },
    dispose() { status.dispose(); },
  }, {});
}

const transparencyComparison = (transparent: boolean): Comparison => ({
  name: `${transparent ? "transparent" : "opaque"} status bar encloses its segment row`,
  terminal: { type: "screen", pattern: "(Sonnet 4\\.5)", group: 1, read: "sameRow", relativeTo: { pattern: "(Sonnet 4\\.5)" } },
  browser: { type: "dom", selector: statusBar, read: "sameRow", relativeTo: modelSegment },
  expected: true,
  proves: [{ entry: "property:TspStatusProps.transparent", effect: "layout", value: transparent }],
});

export const fixtureCases: FixtureCase[] = [{
  id: "status-line-bar",
  covers: ["component:packages/tui/src/status-line/component.ts#StatusLineComponent", "property:TspStatusProps.transparent"],
  create: context => mountStatusLine(context, true, true),
  checkpoints: [
    { name: "production status bar", comparisons: [
      barComparison("model segment", "(Sonnet 4\\.5)", modelSegment, "Sonnet 4.5"),
      barComparison("cost segment", "(\\$0\\.42)", costSegment, "$0.42"),
      barComparison("hook status row", "(Parity hook status)", '[data-omp-role="omp.status.hook"]', "Parity hook status"),
      {
        name: "path segment stays in the left group",
        terminal: { type: "screen", pattern: barPath, group: 1, read: "leftHalf" },
        browser: { type: "dom", selector: pathSegment, read: "leftHalf" },
        expected: true,
      },
      {
        name: "cost segment stays in the right group",
        terminal: { type: "screen", pattern: "(\\$0\\.42)", group: 1, read: "rightHalf" },
        browser: { type: "dom", selector: costSegment, read: "rightHalf" },
        expected: true,
      },
      {
        name: "model icon segment shares the bar row",
        terminal: { type: "screen", pattern: "(Sonnet 4\\.5)", group: 1, read: "sameRow", relativeTo: { pattern: barPath } },
        browser: { type: "dom", selector: modelSegment, read: "sameRow", relativeTo: pathSegment },
        expected: true,
      },
      {
        name: "model span paint",
        terminal: { type: "screen", pattern: "(Sonnet 4\\.5)", group: 1, read: "foreground" },
        browser: { type: "dom", selector: `${modelSegment} span:text-is("Sonnet 4.5")`, read: "css", property: "color" },
        expected: "rgb(0, 180, 255)",
      },
      transparencyComparison(true),
    ] },
    { name: "path action", terminalInput: "p", browserActions: [{ type: "click", selector: pathSegment }], comparisons: [
      callbackValue("real status action", "status-action", "status.path"),
    ] },
  ],
}, {
  id: "prefs-appearance-status-preview",
  covers: [
    "property:TspSegProps.min", "property:TspSegProps.min.w", "property:TspSegProps.icon", "property:TspSegProps.side",
  ],
  create: context => createSettings(context),
  checkpoints: [
    { name: "appearance status preview", comparisons: [
      {
        name: "path segment keeps its minimum width in the preview bar",
        terminal: { type: "screen", pattern: "(?<=Preview:[\\s\\S]*)(workspace)", group: 1, read: "leftHalf" },
        browser: { type: "dom", selector: `[data-omp-page] ${pathSegment}`, read: "leftHalf" },
        expected: true,
        proves: [
          { entry: "property:TspSegProps.min", effect: "layout", value: { w: "12ch" } },
          { entry: "property:TspSegProps.min.w", effect: "layout", value: "12ch" },
          { entry: "property:TspSegProps.side", effect: "layout", value: "left" },
        ],
      },
      {
        name: "context meter sits in the right half of the preview bar",
        terminal: { type: "screen", pattern: "(?<=Preview:[\\s\\S]*)(\\d+%)", group: 1, read: "rightHalf" },
        browser: { type: "dom", selector: '[data-omp-page] [data-omp-role="omp.status.context_pct"]', read: "rightHalf" },
        expected: true,
        proves: [{ entry: "property:TspSegProps.side", effect: "layout", value: "right" }],
      },
      {
        name: "model icon leads the model segment in the preview bar",
        terminal: { type: "screen", pattern: "(?<=Preview:[\\s\\S]*)(\u25D2)(?= parity-coder)", group: 1, read: "leftHalf" },
        browser: { type: "dom", selector: '[data-omp-page] [data-omp-role="omp.status.model"] svg', read: "leftHalf" },
        expected: true,
        proves: [{ entry: "property:TspSegProps.icon", effect: "layout", value: "model" }],
      },
    ] },
  ],
}, {
  id: "status-line-opaque-bar",
  covers: ["component:packages/tui/src/status-line/component.ts#StatusLineComponent", "property:TspStatusProps.transparent"],
  create: context => mountStatusLine(context, false, false, true),
  checkpoints: [
    { name: "opaque status bar", comparisons: [
      barComparison("model segment", "(Sonnet 4\\.5)", modelSegment, "Sonnet 4.5"),
      {
        name: "opaque status bar paint",
        terminal: { type: "screen", pattern: "(Sonnet 4\\.5)", group: 1, read: "background" },
        browser: { type: "dom", selector: statusBar, read: "css", property: "background-color" },
        expected: "rgb(15, 18, 22)",
        proves: [{ entry: "property:TspStatusProps.transparent", effect: "appearance", value: false }],
      },
    ] },
  ],
}, {
  id: "status-line-footer",
  covers: [
    "component:packages/tui/src/status-line/footer.ts#FooterComponent",
    "property:TspSegProps.spans",
  ],
  async create(context) {
    const { FooterComponent } = await context.load<typeof FooterModule>("packages/tui/src/status-line/footer.ts");
    const { createGallerySession } = await context.load<typeof PreviewSessionModule>("packages/coding-agent/src/cli/gallery-fixtures/preview-session.ts");
    const { statusLineHost } = await context.load<typeof StatusLineHostModule>("packages/coding-agent/src/modes/status-line-host.ts");
    const footer = new FooterComponent(createGallerySession(), statusLineHost);
    footer.setExtensionStatus("parity", "Footer extension status");
    return controlledGallery(context, {
      render(width) {
        for (const narrowWidth of narrowFooterWidths) footer.render(narrowWidth);
        return footer.render(width);
      },
      describe() { return footer.describe(); },
      invalidate() { footer.invalidate(); },
      dispose() { footer.dispose(); },
    }, {});
  },
  checkpoints: [
    { name: "production footer", comparisons: [
      barComparison("extension status row", "(Footer extension status)", '[data-omp-role="omp.footer.extensions"]', "Footer extension status"),
      barComparison("path and branch segment", "(parity-main)", '[data-omp-role="omp.footer.path"]', "parity-main"),
      {
        name: "footer input counter stays in the left group",
        terminal: { type: "screen", pattern: "(\u219112K)", group: 1, read: "leftHalf" },
        browser: { type: "dom", selector: '[data-omp-role="omp.footer.input"]', read: "leftHalf" },
        expected: true,
      },
      {
        name: "footer branch span paint",
        terminal: { type: "screen", pattern: "\\((parity-main)\\)", group: 1, read: "foreground" },
        browser: { type: "dom", selector: '[data-omp-role="omp.footer.path"] span:text-is(" (parity-main)")', read: "css", property: "color" },
        expected: "rgb(107, 114, 128)",
        proves: [{ entry: "property:TspSegProps.spans", effect: "appearance", value: [{ t: " (parity-main)", s: "dim" }] }],
      },
    ] },
  ],
}, {
  id: "status-line-footer-long-cwd",
  covers: [
    "component:packages/tui/src/status-line/footer.ts#FooterComponent",
  ],
  async create(context) {
    const { FooterComponent } = await context.load<typeof FooterModule>("packages/tui/src/status-line/footer.ts");
    const { createGallerySession } = await context.load<typeof PreviewSessionModule>("packages/coding-agent/src/cli/gallery-fixtures/preview-session.ts");
    const { statusLineHost } = await context.load<typeof StatusLineHostModule>("packages/coding-agent/src/modes/status-line-host.ts");
    const dirs = await context.load<typeof DirsModule>("packages/utils/src/dirs.ts");
    const previous = dirs.getProjectDir();
    // Longer than the 120-column terminal: the terminal footer keeps the path on its own row, so the model must still show.
    const longDirectory = join(context.workspace, ...Array.from({ length: 6 }, (_, index) => `segment-directory-${index}-with-a-long-name`));
    await mkdir(longDirectory, { recursive: true });
    dirs.setProjectDir(longDirectory);
    const footer = new FooterComponent(createGallerySession(), statusLineHost);
    return controlledGallery(context, {
      render(width) { return footer.render(width); },
      describe() { return footer.describe(); },
      invalidate() { footer.invalidate(); },
      dispose() { footer.dispose(); dirs.setProjectDir(previous); },
    }, {});
  },
  checkpoints: [
    { name: "long working directory", comparisons: [
      barComparison("model stays visible", "(claude-sonnet-4-5 \u2022 high)", '[data-omp-role="omp.footer.model"]', "claude-sonnet-4-5 \u2022 high"),
      {
        name: "footer model stays in the right group",
        terminal: { type: "screen", pattern: "(claude-sonnet-4-5)", group: 1, read: "rightHalf" },
        browser: { type: "dom", selector: '[data-omp-role="omp.footer.model"]', read: "rightHalf" },
        expected: true,
      },
    ] },
  ],
}, {
  id: "segment-pi-working",
  covers: ["component:packages/tui/src/status-line/segments.ts#piSegment"],
  async create(context) {
    const production = await context.load<typeof SegmentsModule>("packages/tui/src/status-line/segments.ts");
    const gallery = await context.load<typeof SegmentGalleryModule>("packages/coding-agent/src/cli/gallery-fixtures/segments.ts");
    const { node } = await context.load<typeof DescribeModule>("packages/tui/src/native/describe.ts");
    const dirs = await context.load<typeof DirsModule>("packages/utils/src/dirs.ts");
    dirs.setProfile("segmentprofile");
    const segmentContext = gallery.createGallerySegmentContext();
    segmentContext.turnElapsedMs = 2_000;
    segmentContext.now = new Date(2_000);
    if (!production.renderSegment("pi", segmentContext).visible || !production.describeSegment("pi", segmentContext)) throw new Error("Working pi segment unexpectedly hidden");
    return controlledGallery(context, {
      render() { return [production.renderSegment("pi", segmentContext).content]; },
      describe() {
        const view = production.describeSegment("pi", segmentContext);
        if (!view) throw new Error("Missing native pi segment");
        return node("status", { transparent: true }, [node("seg", {
          role: "parity.segment.pi-working", spans: view.spans, icon: view.icon, tone: view.tone, title: view.title,
        }, view.motion, "pi")]);
      },
      invalidate() {},
      dispose() { dirs.setProfile(undefined); },
    }, {});
  },
  checkpoints: [
    { name: "working brand segment", comparisons: [barComparison("active profile tag", "(segmentprofile)", '[data-omp-role="parity.segment.pi-working"]', "segmentprofile"), {
      name: "brand segment visible on both interfaces",
      terminal: { type: "screen", pattern: "(2s)", group: 1, read: "visible" },
      browser: { type: "dom", selector: '[data-omp-role="parity.segment.pi-working"]', read: "visible" },
      expected: true,
    }] },
  ],
}];
