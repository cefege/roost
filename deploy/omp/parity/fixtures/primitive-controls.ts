import type * as InputModule from "@omp-source/packages/tui/src/components/input";
import type * as SelectModule from "@omp-source/packages/tui/src/components/select-list";
import type * as ThemeModule from "@omp-source/packages/tui/src/theme/tui-adapters";
import type * as TabModule from "@omp-source/packages/tui/src/components/tab-bar";
import type * as ProgressModule from "@omp-source/packages/tui/src/components/progress-bar";
import type * as MetricModule from "@omp-source/packages/tui/src/components/metric";
import type * as BoxModule from "@omp-source/packages/tui/src/components/box";
import type * as LoaderModule from "@omp-source/packages/tui/src/components/loader";
import type * as ShimmerModule from "@omp-source/packages/tui/src/theme/shimmer";
import type * as ImageModule from "@omp-source/packages/tui/src/components/image";
import type { FixtureCase } from "../scenario";
import { callbackValue } from "../scenario";
import { contentComparison, controlledGallery } from "./gallery-support";

const component = (file: string, name: string) => `component:packages/tui/src/components/${file}.ts#${name}`;
const tabStrip = '[data-omp-node="l.1"]';
const tabItems = [{ id: "files", label: "Files view" }, { id: "muted", label: [{ t: "Unavailable view", s: "muted" }] }, { id: "history", label: "History view" }];
const surface = '[id^="omp-view-"][data-omp-surface]';
const listWindow = '[data-omp-node="l.list"] > div';
const alphaRow = '[data-omp-node="l.list/alpha"] button';
const field = '[data-omp-node] input:not([type="file"])';
const retainedMetric = '[data-omp-overlay="nonmodal"]:not(:has(:text-is("Cost $0.42"):visible)) [data-omp-node]:not(:has([data-omp-node])):text-is("Tokens 1234")';
const metricStatusOwner = '[data-omp-node]:has(> [data-omp-status="true"])';
const metricOrigin = '[data-omp-status="true"] > [data-omp-node]:has-text("Tokens 1234")';

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-input-unicode-edit-submit",
    covers: ["kind:input", "property:TspEditorProps.text", "property:TspEditorProps.prompt", component("input", "Input")],
    async create(context) {
      const { Input } = await context.load<typeof InputModule>("packages/tui/src/components/input.ts");
      const input = new Input();
      input.prompt = "Name:";
      input.setValue("Café 🧑🏽‍💻 中");
      input.onSubmit = value => context.record("submitted", value);
      return input;
    },
    checkpoints: [
      { name: "native Unicode value", comparisons: [
        { name: "editable value", terminal: { type: "screen", pattern: "(Café 🧑🏽‍💻 中)", group: 1 }, browser: { type: "dom", selector: field, read: "value" }, expected: "Café 🧑🏽‍💻 中", proves: [{ entry: "property:TspEditorProps.text", effect: "literal", value: "Café 🧑🏽‍💻 中" }] },
        contentComparison("input prompt", "(Name:)", '[data-omp-native-editor] label', "Name:", [{ entry: "property:TspEditorProps.prompt", effect: "literal", value: "Name:" }]),
      ] },
      { name: "replace and submit through actual Input", terminalInput: "\x01\x0bRevised 日本語\r", browserActions: [{ type: "fill", selector: field, value: "Revised 日本語" }, { type: "press", selector: field, key: "Enter" }], comparisons: [
        callbackValue("submitted value", "submitted", "Revised 日本語"),
        { name: "controller retained submitted value", terminal: { type: "screen", pattern: "(Revised 日本語)", group: 1 }, browser: { type: "dom", selector: field, read: "value" }, expected: "Revised 日本語", proves: [{ entry: "property:TspEditorProps.text", effect: "literal", value: "Revised 日本語" }] },
      ] },
    ],
  },
  {
    id: "primitive-masked-input-edit",
    covers: ["property:TspEditorProps.mask", "property:TspEditorProps.text", component("input", "Input")],
    async create(context) {
      const { Input } = await context.load<typeof InputModule>("packages/tui/src/components/input.ts");
      const input = new Input();
      input.prompt = "Secret: ";
      input.mask = true;
      input.setValue("alpha");
      input.onSubmit = value => context.record("secret-submitted", value);
      return input;
    },
    checkpoints: [
      { name: "stored secret is display-only mask", comparisons: [{ name: "five masked graphemes", terminal: { type: "screen", pattern: "(?:^|[^•])(•••••)(?!•)", group: 1 }, browser: { type: "dom", selector: field, read: "attribute", attribute: "placeholder" }, expected: "•••••", proves: [{ entry: "property:TspEditorProps.text", effect: "literal", value: "•••••" }] }] },
      { name: "new Unicode secret reaches original controller", terminalInput: "\x01\x0bΔ🧑🏽‍💻\r", browserActions: [{ type: "fill", selector: field, value: "Δ🧑🏽‍💻" }, { type: "press", selector: field, key: "Enter" }], comparisons: [
        { ...callbackValue("private callback value", "secret-submitted", "Δ🧑🏽‍💻"), proves: [{ entry: "property:TspEditorProps.mask", effect: "interaction", value: true, target: { checkpoint: "new Unicode secret reaches original controller", comparison: "updated masked display" }, assertionGroups: [{ comparisons: [{ checkpoint: "new Unicode secret reaches original controller", comparison: "private callback value" }, { checkpoint: "new Unicode secret reaches original controller", comparison: "updated masked display" }] }] }] },
        { name: "updated masked display", terminal: { type: "screen", pattern: "(?:^|[^•])(••)(?!•)", group: 1 }, browser: { type: "dom", selector: field, read: "attribute", attribute: "placeholder" }, expected: "••", proves: [{ entry: "property:TspEditorProps.text", effect: "literal", value: "••" }] },
      ] },
    ],
  },
  {
    id: "primitive-list-disabled-navigation",
    covers: ["kind:list", "kind:item", "property:TspListProps.selected", "property:TspListProps.max", "property:TspListProps.max.lines", "property:TspItemProps.label", "property:TspItemProps.detail", "property:TspItemProps.disabled", "property:TspSpan.t", component("select-list", "SelectList")],
    async create(context) {
      const { SelectList } = await context.load<typeof SelectModule>("packages/tui/src/components/select-list.ts");
      const { getSelectListTheme } = await context.load<typeof ThemeModule>("packages/tui/src/theme/tui-adapters.ts");
      const theme = getSelectListTheme();
      const list = new SelectList([{ value: "alpha", label: "Alpha choice", description: "First available" }, { value: "disabled", label: "Disabled choice", description: "Unavailable", disabled: true }, { value: "gamma", label: "Gamma choice", description: "Last available" }], 5, { ...theme, selectedText: text => theme.selectedText(`\x1b[7m${text}\x1b[27m`) }, { search: "never" });
      list.onSelect = item => context.record("selection", item.value);
      return list;
    },
    checkpoints: [
      { name: "disabled row remains visible", comparisons: [
        contentComparison("disabled label", "(Disabled choice)", 'button:disabled:has-text("Disabled choice")', "Disabled choice", [{ entry: "property:TspSpan.t", effect: "literal", value: "Disabled choice" }]),
        contentComparison("disabled explanation", "(Unavailable)", 'button:disabled:has-text("Disabled choice")', "Unavailable", [{ entry: "property:TspSpan.t", effect: "literal", value: "Unavailable" }]),
        { name: "list window sits in the lower half", terminal: { type: "screen", pattern: "Alpha choice", read: "bottomHalf" }, browser: { type: "dom", selector: listWindow, read: "bottomHalf", relativeTo: surface }, expected: true, proves: [{ entry: "property:TspListProps.max", effect: "layout", value: { lines: 5 } }, { entry: "property:TspListProps.max.lines", effect: "layout", value: 5 }] },
        { name: "label sits in the lower list window", terminal: { type: "screen", pattern: "Alpha choice", read: "bottomHalf" }, browser: { type: "dom", selector: `${alphaRow} > span:nth-child(1)`, read: "bottomHalf", relativeTo: surface }, expected: true, proves: [{ entry: "property:TspItemProps.label", effect: "layout", value: [{ t: "Alpha choice" }] }] },
        { name: "detail sits in the lower list window", terminal: { type: "screen", pattern: "First available", read: "bottomHalf" }, browser: { type: "dom", selector: `${alphaRow} > span:nth-child(2)`, read: "bottomHalf", relativeTo: surface }, expected: true, proves: [{ entry: "property:TspItemProps.detail", effect: "layout", value: [{ t: "First available", s: "muted" }] }] },
      ] },
      { name: "skip disabled and activate", terminalInput: "\x1b[B\r", browserActions: [{ type: "click", selector: '[data-omp-node$="gamma"]' }], comparisons: [
        { ...callbackValue("selected enabled item", "selection", "gamma"), proves: [{ entry: "property:TspItemProps.disabled", effect: "interaction", value: true, target: { checkpoint: "skip disabled and activate", comparison: "disabled item remains ineligible" }, assertionGroups: [{ comparisons: [{ checkpoint: "skip disabled and activate", comparison: "selected enabled item" }, { checkpoint: "skip disabled and activate", comparison: "disabled item remains ineligible" }] }] }] },
        contentComparison("disabled item remains ineligible", "(Disabled choice)", 'button:disabled:has-text("Disabled choice")', "Disabled choice"),
        { name: "selected gamma is genuinely highlighted", terminal: { type: "screen", pattern: "❯ (Gamma choice)", group: 1 }, browser: { type: "dom", selector: '[data-omp-node="l.list/gamma"] [data-omp-item-selected="true"] button', read: "text", pattern: "(Gamma choice)", group: 1 }, expected: "Gamma choice", normalize: "whitespace" },
        { name: "list window keeps the lower-half anchor", terminal: { type: "screen", pattern: "Gamma choice", read: "bottomHalf" }, browser: { type: "dom", selector: listWindow, read: "bottomHalf", relativeTo: surface }, expected: true },
        { name: "selection moved below the first row", terminal: { type: "screen", pattern: "Gamma choice", relativeTo: { pattern: "Alpha choice" }, read: "below" }, browser: { type: "dom", selector: '[data-omp-node="l.list/gamma"] [data-omp-item-selected="true"] button', relativeTo: alphaRow, read: "below" }, expected: true, proves: [{ entry: "property:TspListProps.selected", effect: "layout", value: "l.list/gamma", target: { checkpoint: "skip disabled and activate", comparison: "list window keeps the lower-half anchor" } }] },
      ] },
    ],
  },
  {
    id: "primitive-list-filter-no-match",
    covers: ["property:TspListProps.filter", "property:TspListProps.empty", component("select-list", "SelectList")],
    async create(context) {
      const { SelectList } = await context.load<typeof SelectModule>("packages/tui/src/components/select-list.ts");
      const { getSelectListTheme } = await context.load<typeof ThemeModule>("packages/tui/src/theme/tui-adapters.ts");
      const list = new SelectList([{ value: "alpha", label: "Alpha filtered" }], 5, getSelectListTheme(), { search: "always", noMatchText: "No fixture matches" });
      list.setFilter("unmatched");
      return list;
    },
    checkpoints: [{ name: "production no-match state", comparisons: [
      contentComparison("no match message", "(No fixture matches)", 'text="No fixture matches"', "No fixture matches", [{ entry: "property:TspListProps.empty", effect: "literal", value: "No fixture matches" }]),
      { name: "filtered list reports its empty state in the lower half", terminal: { type: "screen", pattern: "No fixture matches", read: "bottomHalf" }, browser: { type: "dom", selector: 'text="No fixture matches"', read: "bottomHalf", relativeTo: surface }, expected: true, proves: [{ entry: "property:TspListProps.filter", effect: "layout", value: "unmatched" }] },
    ] }],
  },
  {
    id: "primitive-tab-switching",
    covers: ["kind:tabs", "property:TspTabsProps.items", "property:TspTabsProps.active", "property:TspTabsProps.items[].id", component("tab-bar", "TabBar")],
    async create(context) {
      const { TabBar } = await context.load<typeof TabModule>("packages/tui/src/components/tab-bar.ts");
      const tab = new TabBar("Views", [{ id: "files", label: "Files view" }, { id: "muted", label: "Unavailable view", muted: true }, { id: "history", label: "History view" }], {
        label: text => context.theme.fg("dim", text), activeTab: text => context.theme.fg("accent", `\x1b[7m${text}\x1b[27m`), inactiveTab: text => context.theme.fg("muted", text), hint: text => context.theme.fg("dim", text),
      });
      tab.onTabChange = selected => context.record("tab", selected.id);
      return tab;
    },
    checkpoints: [
      { name: "initial active tab", comparisons: [
        contentComparison("active files label", "(Files view)", `${tabStrip} [role="tab"][aria-selected="true"]`, "Files view"),
        { name: "files are selected on both live interfaces", terminal: { type: "screen", pattern: "(Files view)", group: 1, read: "inverse" }, browser: { type: "dom", selector: `${tabStrip} [role="tab"][data-omp-item="files"]`, read: "selected" }, expected: true },
        { name: "tabs share one row", terminal: { type: "screen", pattern: "History view", relativeTo: { pattern: "Files view" }, read: "sameRow" }, browser: { type: "dom", selector: `${tabStrip} [data-omp-item="history"]`, relativeTo: `${tabStrip} [data-omp-item="files"]`, read: "sameRow" }, expected: true, proves: [{ entry: "property:TspTabsProps.items", effect: "layout", value: tabItems }, { entry: "property:TspTabsProps.items[].id", effect: "layout", value: "history" }] },
        { name: "active tab precedes the others", terminal: { type: "screen", pattern: "Files view", read: "leftHalf" }, browser: { type: "dom", selector: `${tabStrip} [role="tab"][aria-selected="true"]`, read: "leftHalf", relativeTo: surface }, expected: true, proves: [{ entry: "property:TspTabsProps.active", effect: "layout", value: "files" }] },
      ] },
      { name: "switch skipping muted", terminalInput: "\t", browserActions: [{ type: "click", selector: `${tabStrip} [role="tab"][data-omp-item="history"]` }], comparisons: [
        callbackValue("selected tab identity", "tab", "history"),
        contentComparison("active history label", "(History view)", `${tabStrip} [role="tab"][aria-selected="true"]`, "History view"),
        { name: "history is selected on both live interfaces", terminal: { type: "screen", pattern: "(History view)", group: 1, read: "inverse" }, browser: { type: "dom", selector: `${tabStrip} [role="tab"][data-omp-item="history"]`, read: "selected" }, expected: true },
      ] },
    ],
  },
  {
    id: "primitive-progress-fraction",
    covers: ["kind:progress", "property:TspProgressProps.value", "property:TspProgressProps.label", component("progress-bar", "ProgressBar")],
    async create(context) {
      const { ProgressBar } = await context.load<typeof ProgressModule>("packages/tui/src/components/progress-bar.ts");
      const progress = new ProgressBar(42, { min: 0, max: 100, showPercentage: true, formatPercentage: fraction => `${Math.round(fraction * 100)}% (${fraction.toFixed(2)})`, maxWidth: 20, style: { filled: "█", empty: "░" } });
      return controlledGallery(context, progress, { "u update progress": () => { progress.setValue(84); context.record("progress-value", 0.84); } });
    },
    checkpoints: [
      { name: "normalized progress value", comparisons: [
        { name: "42 percent", terminal: { type: "screen", pattern: "(42)%", group: 1 }, browser: { type: "dom", selector: '[role="progressbar"]', read: "attribute", attribute: "aria-valuenow" }, expected: 42, normalize: "number" },
        { name: "exact rendered fraction", terminal: { type: "screen", pattern: "(0\\.42)", group: 1 }, browser: { type: "dom", selector: '[role="progressbar"]', read: "attribute", attribute: "aria-label", pattern: "(0\\.42)", group: 1 }, expected: 0.42, normalize: "number", proves: [{ entry: "property:TspProgressProps.value", effect: "literal", value: 0.42 }] },
        { name: "complete progress label", terminal: { type: "screen", pattern: "(42% \\(0\\.42\\))", group: 1 }, browser: { type: "dom", selector: '[role="progressbar"]', read: "attribute", attribute: "aria-label" }, expected: "42% (0.42)", proves: [{ entry: "property:TspProgressProps.label", effect: "literal", value: "42% (0.42)" }] },
      ] },
      { name: "progress advances through the real setter", terminalInput: "u", browserActions: [{ type: "click", selector: 'button:text-is("u update progress")' }], comparisons: [
        callbackValue("progress update callback", "progress-value", 0.84),
        { name: "84 percent", terminal: { type: "screen", pattern: "(84)%", group: 1 }, browser: { type: "dom", selector: '[role="progressbar"]', read: "attribute", attribute: "aria-valuenow" }, expected: 84, normalize: "number" },
        { name: "advanced rendered fraction", terminal: { type: "screen", pattern: "(0\\.84)", group: 1 }, browser: { type: "dom", selector: '[role="progressbar"]', read: "attribute", attribute: "aria-label", pattern: "(0\\.84)", group: 1 }, expected: 0.84, normalize: "number", proves: [{ entry: "property:TspProgressProps.value", effect: "literal", value: 0.84 }] },
        { name: "advanced complete label", terminal: { type: "screen", pattern: "(84% \\(0\\.84\\))", group: 1 }, browser: { type: "dom", selector: '[role="progressbar"]', read: "attribute", attribute: "aria-label" }, expected: "84% (0.84)", proves: [{ entry: "property:TspProgressProps.label", effect: "literal", value: "84% (0.84)" }] },
      ] },
    ],
  },
  {
    id: "primitive-status-priority-overflow",
    covers: ["kind:status", "kind:seg", "property:TspSegProps.priority", component("metric", "MetricRow")],
    presentation: { overlay: true, overlayOptions: { width: 17, minWidth: 17, row: 2, col: 0 } },
    async create(context) {
      const { MetricRow } = await context.load<typeof MetricModule>("packages/tui/src/components/metric.ts");
      return new MetricRow([{ leading: "Tokens", value: "1234", priority: 10 }, { leading: "Cost", value: "$0.42", priority: 1 }], { overflow: "drop", separator: " · " });
    },
    checkpoints: [{ name: "constrained status retains only the highest priority", comparisons: [
      { name: "low priority is absent while high priority stays visible", terminal: { type: "screen", pattern: "^(?![\\s\\S]*Cost \\$0\\.42)[\\s\\S]*(Tokens 1234)[\\s\\S]*$", group: 1 }, browser: { type: "dom", selector: retainedMetric, read: "text" }, expected: "Tokens 1234", normalize: "whitespace" },
      { name: "retained metric occupies the available row origin", terminal: { type: "screen", pattern: "(Tokens 1234)", group: 1, read: "column" }, browser: { type: "dom", selector: retainedMetric, read: "rectX", relativeTo: '[id^="omp-view-"][data-omp-surface]', unit: "cells" }, expected: 0, proves: [{ entry: "property:TspSegProps.priority", effect: "layout", value: 10, assertionGroups: [{ comparisons: [{ checkpoint: "constrained status retains only the highest priority", comparison: "low priority is absent while high priority stays visible" }, { checkpoint: "constrained status retains only the highest priority", comparison: "retained metric occupies the available row origin" }] }] }] },
    ] }],
  },
  {
    id: "primitive-metric-status-segments",
    covers: ["kind:status", "kind:seg", "property:TspStatusProps.transparent", "property:TspSegProps.spans", "property:TspSegProps.priority", component("metric", "MetricRow"), component("box", "Box")],
    presentation: { overlay: true, overlayOptions: { width: 64, minWidth: 64, row: 0, col: 0 } },
    async create(context) {
      const [{ MetricRow }, { Box }] = await Promise.all([
        context.load<typeof MetricModule>("packages/tui/src/components/metric.ts"),
        context.load<typeof BoxModule>("packages/tui/src/components/box.ts"),
      ]);
      const metrics = [{ leading: "Tokens", value: "1234", priority: 10 }, { leading: "Cost", value: "$0.42", priority: 1 }];
      const options: MetricModule.MetricRowOptions = { overflow: "drop", separator: " · " };
      const root = new Box(0, 0);
      root.addChild(new MetricRow(metrics, { ...options, paddingX: 0, paddingY: 0 }));
      return controlledGallery(context, root, { "p normalized metric padding": () => {
        root.clear();
        root.addChild(new MetricRow(metrics, { ...options, paddingX: 2.9, paddingY: 0.25 }));
      } });
    },
    checkpoints: [
      { name: "metric strip contents", comparisons: [
        contentComparison("token metric", "(Tokens 1234)", 'text="Tokens 1234"', "Tokens 1234"),
        contentComparison("cost metric", "(Cost \\$0\\.42)", 'text="Cost $0.42"', "Cost $0.42"),
        contentComparison("whole metric row", "(Tokens 1234 · Cost \\$0\\.42)", metricStatusOwner, "Tokens 1234 · Cost $0.42"),
        { name: "literal rendered metric separator", terminal: { type: "screen", pattern: "Tokens 1234( · )Cost \\$0\\.42", group: 1 }, browser: { type: "dom", selector: '[data-omp-status-separator="0"] > span > span', read: "text" }, expected: " · " },
        { name: "unpadded horizontal origin", terminal: { type: "screen", pattern: "Tokens 1234", read: "column" }, browser: { type: "dom", selector: metricOrigin, relativeTo: '[data-omp-status="true"]', read: "rectX" }, expected: 0 },
        { name: "unpadded vertical origin", terminal: { type: "screen", pattern: "Tokens 1234", read: "row" }, browser: { type: "dom", selector: metricOrigin, relativeTo: '[data-omp-status="true"]', read: "rectY" }, expected: 0 },
      ] },
      { name: "actual metric padding normalizes and moves contents", terminalInput: "p", browserActions: [{ type: "click", selector: 'button:has-text("p normalized metric padding")' }], comparisons: [
        contentComparison("whole padded metric row", "(Tokens 1234 · Cost \\$0\\.42)", metricStatusOwner, "Tokens 1234 · Cost $0.42"),
        { name: "horizontal padding adds actual extent", terminal: { type: "screen", pattern: "Tokens 1234", read: "column" }, browser: { type: "dom", selector: metricOrigin, relativeTo: '[data-omp-status="true"]', read: "rectX" }, temporal: { direction: "increase", from: { checkpoint: "metric strip contents", comparison: "unpadded horizontal origin" } } },
        { name: "vertical padding adds actual extent", terminal: { type: "screen", pattern: "Tokens 1234", read: "row" }, browser: { type: "dom", selector: metricOrigin, relativeTo: '[data-omp-status="true"]', read: "rectY" }, temporal: { direction: "increase", from: { checkpoint: "metric strip contents", comparison: "unpadded vertical origin" } } },
      ] },
    ],
  },
  {
    id: "primitive-spinner-shimmer",
    covers: ["kind:spinner", "kind:shimmer", "property:TspSpinnerProps.style", "property:TspShimmerProps.spans", component("loader", "Loader")],
    async create(context) {
      const { Loader } = await context.load<typeof LoaderModule>("packages/tui/src/components/loader.ts");
      const { shimmerText } = await context.load<typeof ShimmerModule>("packages/tui/src/theme/shimmer.ts");
      const color = Object.assign((text: string) => shimmerText(text, context.theme), { animated: true as const });
      return new Loader(context.tui, text => context.theme.fg("accent", text), color, "Indexing fixture sources");
    },
    checkpoints: [{ name: "animated loader message", comparisons: [contentComparison("loader content", "(Indexing fixture sources)", '[data-omp-role="omp.loader"] .shimmer', "Indexing fixture sources")] }],
  },
  {
    id: "primitive-content-addressed-image",
    covers: ["kind:image", "property:TspImageProps.blob", "property:TspImageProps.alt", "property:TspImageProps.w", "property:TspImageProps.h", component("image", "Image")],
    async create(context) {
      const { Image } = await context.load<typeof ImageModule>("packages/tui/src/components/image.ts");
      return new Image("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aN5sAAAAASUVORK5CYII=", "image/png", { fallbackColor: text => context.theme.fg("dim", text) }, { filename: "parity-pixel.png", maxWidthCells: 8, maxHeightCells: 4 });
    },
    checkpoints: [{ name: "image fallback matches accessible HTML image", comparisons: [
      { name: "image filename", terminal: { type: "screen", pattern: "(parity-pixel\\.png)", group: 1 }, browser: { type: "dom", selector: 'img[alt*="parity-pixel.png"]', read: "attribute", attribute: "alt", pattern: "(parity-pixel\\.png)", group: 1 }, expected: "parity-pixel.png" },
      { name: "complete image alternative", terminal: { type: "screen", pattern: "(\\[Image: parity-pixel\\.png \\[image/png\\] 1x1\\])", group: 1 }, browser: { type: "dom", selector: 'img[alt*="parity-pixel.png"]', read: "attribute", attribute: "alt" }, expected: "[Image: parity-pixel.png [image/png] 1x1]", proves: [{ entry: "property:TspImageProps.alt", effect: "literal", value: "[Image: parity-pixel.png [image/png] 1x1]" }] },
      { name: "decoded image natural width", terminal: { type: "screen", pattern: "\\[Image: parity-pixel\\.png \\[image/png\\] (1)x1\\]", group: 1 }, browser: { type: "dom", selector: 'img[alt*="parity-pixel.png"]', read: "naturalWidth" }, normalize: "number", expected: 1, proves: [{ entry: "property:TspImageProps.w", effect: "literal", value: 1 }] },
      { name: "decoded image natural height", terminal: { type: "screen", pattern: "\\[Image: parity-pixel\\.png \\[image/png\\] 1x(1)\\]", group: 1 }, browser: { type: "dom", selector: 'img[alt*="parity-pixel.png"]', read: "naturalHeight" }, normalize: "number", expected: 1, proves: [{ entry: "property:TspImageProps.h", effect: "literal", value: 1 }] },
    ] }],
  },
];
