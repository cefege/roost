import type * as TextModule from "@omp-source/packages/tui/src/components/text";
import type * as RowModule from "@omp-source/packages/tui/src/components/layout/row";
import type * as TuiModule from "@omp-source/packages/tui/src/tui";
import type * as InputModule from "@omp-source/packages/tui/src/components/input";
import type * as EditorModule from "@omp-source/packages/tui/src/components/editor";
import type * as AutocompleteModule from "@omp-source/packages/tui/src/autocomplete";
import type * as ThemeModule from "@omp-source/packages/tui/src/theme/tui-adapters";
import type * as PlanSaveModule from "@omp-source/packages/tui/src/overlays/plan-save-overlay";
import { callbackValue, type Comparison, type FixtureCase, type FixtureContext } from "../scenario";

const surface = '[id^="omp-view-"][data-omp-surface]';
const panel = '[data-omp-overlay="nonmodal"]';
const input = '[data-omp-node] input:not([type="file"])';
const editorInput = '[data-omp-role="omp.field"] textarea';
const autocomplete = '[data-omp-role="omp.autocomplete"]';
const leaf = (marker: string) => `${panel} [data-omp-node]:not(:has([data-omp-node])):has-text("${marker}")`;
const component = (path: string, name: string) => `component:packages/tui/src/${path}.ts#${name}`;
const structure = [component("components/layout/row", "Row"), component("tui", "Container")];

type Options = TuiModule.OverlayOptions;

function location(marker: string, read: "leftHalf" | "rightHalf" | "topHalf" | "bottomHalf", expected: boolean): Comparison {
  return { name: `${marker} ${read}`, terminal: { type: "screen", pattern: marker, read }, browser: { type: "dom", selector: leaf(marker), read, relativeTo: surface }, expected };
}

function coordinate(marker: string, axis: "column" | "row", expected: number): Comparison {
  return { name: `${marker} ${axis}`, terminal: { type: "screen", pattern: marker, read: axis }, browser: { type: "dom", selector: leaf(marker), read: axis === "column" ? "rectX" : "rectY", relativeTo: surface, unit: "cells" }, expected };
}

function inset(marker: string, read: "rightGap" | "bottomGap", expected: number): Comparison {
  return { name: `${marker} ${read}`, terminal: { type: "screen", pattern: marker, read }, browser: { type: "dom", selector: leaf(marker), read, relativeTo: surface, unit: "cells" }, expected };
}

function contained(marker: string, expected = true, relativeTo = surface): Comparison {
  return { name: `${marker} contained`, terminal: { type: "screen", pattern: marker, read: "visible" }, browser: { type: "dom", selector: leaf(marker), read: "insideViewport", relativeTo }, expected };
}

async function geometryContent(context: FixtureContext) {
  const [{ Text }, { Row }, { Container }] = await Promise.all([
    context.load<typeof TextModule>("packages/tui/src/components/text.ts"),
    context.load<typeof RowModule>("packages/tui/src/components/layout/row.ts"),
    context.load<typeof TuiModule>("packages/tui/src/tui.ts"),
  ]);
  const content = new Container();
  for (const [left, right] of [["LQ", "RQ"], ["MQ", "NQ"], ["BQ", "EQ"]]) {
    content.addChild(new Row({ children: [
      { content: new Text(left, 0, 0), width: 2 },
      { content: new Text("", 0, 0), grow: 1 },
      { content: new Text(right, 0, 0), width: 2 },
    ] }));
  }
  return content;
}

function geometryCase(id: string, options: Options, comparisons: Comparison[]): FixtureCase {
  return { id: `overlay-${id}`, covers: structure, presentation: { overlay: true, overlayOptions: options }, create: geometryContent, checkpoints: [{ name: "positioned real layout", comparisons }] };
}

const anchors = ["center", "top-left", "top-right", "top-center", "bottom-left", "bottom-right", "bottom-center", "left-center", "right-center"] as const;

export const fixtureCases: FixtureCase[] = [
  ...anchors.map(anchor => {
    const horizontal = anchor.includes("left") ? [true, false] : anchor.includes("right") ? [false, true] : [true, true];
    const vertical = anchor.startsWith("top") ? [true, false] : anchor.startsWith("bottom") ? [false, true] : [true, true];
    return geometryCase(`anchor-${anchor}`, { anchor, width: 20, margin: 0 }, [
      location("LQ", "leftHalf", horizontal[0]!), location("RQ", "rightHalf", horizontal[1]!),
      location("LQ", "topHalf", vertical[0]!), location("BQ", "bottomHalf", vertical[1]!),
      contained("LQ"), contained("EQ"),
    ]);
  }),
  geometryCase("cell-width", { anchor: "top-left", width: 24, margin: 0 }, [coordinate("LQ", "column", 0), coordinate("RQ", "column", 22)]),
  geometryCase("minimum-width", { anchor: "top-left", width: 12, minWidth: 42, margin: 0 }, [coordinate("LQ", "column", 0), coordinate("RQ", "column", 40)]),
  geometryCase("absolute-cell-row", { anchor: "bottom-right", width: 20, row: 3, col: 7, margin: 0 }, [coordinate("LQ", "row", 3), coordinate("LQ", "column", 7), coordinate("EQ", "row", 5)]),
  geometryCase("offset-cells-rows", { anchor: "top-left", width: 20, offsetX: 9, offsetY: 4, margin: 0 }, [coordinate("LQ", "column", 9), coordinate("LQ", "row", 4)]),
  geometryCase("offset-collision", { anchor: "top-left", width: 20, offsetX: -90, offsetY: -90, margin: 0 }, [coordinate("LQ", "column", 0), coordinate("LQ", "row", 0)]),
  geometryCase("uniform-margin", { anchor: "top-left", width: 20, margin: 3 }, [coordinate("LQ", "column", 3), coordinate("LQ", "row", 3)]),
  geometryCase("asymmetric-margin", { anchor: "top-left", width: 20, margin: { top: 2, left: 5, bottom: 4, right: 8 } }, [coordinate("LQ", "column", 5), coordinate("LQ", "row", 2)]),
  geometryCase("asymmetric-bottom-right", { anchor: "bottom-right", width: 20, margin: { top: 2, left: 5, bottom: 4, right: 8 } }, [inset("RQ", "rightGap", 8), inset("EQ", "bottomGap", 4), contained("EQ")]),
  geometryCase("percentage-position-start", { width: 20, row: "0%", col: "0%", margin: 0 }, [coordinate("LQ", "column", 0), coordinate("LQ", "row", 0)]),
  geometryCase("percentage-position-end", { width: 20, row: "100%", col: "100%", margin: 2 }, [location("LQ", "rightHalf", true), location("LQ", "bottomHalf", true), contained("EQ")]),
  geometryCase("percentage-width", { anchor: "top-left", width: "75%", margin: 0 }, [location("LQ", "leftHalf", true), location("RQ", "rightHalf", true), contained("RQ")]),
  ...([1, 2] as const).map(maxHeight => geometryCase(`height-${maxHeight}`, { anchor: "top-left", width: 20, maxHeight, margin: 0 }, [contained("LQ", true, panel), contained("MQ", maxHeight === 2, panel), contained("BQ", false, panel)])),
  geometryCase("percentage-height", { anchor: "top-left", width: 20, maxHeight: "1%", margin: 0 }, [contained("LQ", true, panel), contained("MQ", false, panel)]),
  {
    ...geometryCase("phone-collision", { anchor: "bottom-right", width: 200, minWidth: 200, offsetX: 500, offsetY: 500, margin: 2 }, []),
    checkpoints: [{ name: "oversized panel clamps after phone resize", terminalResize: { columns: 40, rows: 18 }, browserActions: [{ type: "resize", width: 390, height: 480 }], comparisons: [coordinate("LQ", "column", 2), contained("LQ"), contained("EQ"), location("BQ", "bottomHalf", true)] }],
  },
  {
    id: "overlay-passive-layers-keep-input",
    covers: [...structure, component("components/input", "Input")],
    async create(context) {
      const [{ Input }, { Text }, { Container }] = await Promise.all([
        context.load<typeof InputModule>("packages/tui/src/components/input.ts"),
        context.load<typeof TextModule>("packages/tui/src/components/text.ts"),
        context.load<typeof TuiModule>("packages/tui/src/tui.ts"),
      ]);
      const field = new Input();
      field.prompt = "Layer action: ";
      const layers: TuiModule.OverlayHandle[] = [];
      field.onSubmit = value => {
        if (value === "clear") { for (const layer of layers.splice(0).reverse()) layer.hide(); }
        else {
          const group = new Container();
          group.addChild(new Text(`${value}-first`, 0, 0));
          group.addChild(new Text(`${value}-second`, 0, 0));
          layers.push(context.tui.showOverlay(group, { anchor: value === "alpha" ? "top-left" : "top-right", width: 24, margin: 1 }));
        }
        context.tui.setFocus(field);
        field.setValue("");
        context.record("layer-count", layers.length);
        context.tui.requestRender();
      };
      return field;
    },
    checkpoints: [
      { name: "first passive group", terminalInput: "alpha\r", browserActions: [{ type: "fill", selector: input, value: "alpha" }, { type: "press", selector: input, key: "Enter" }], comparisons: [callbackValue("first layer", "layer-count", 1), location("alpha-first", "leftHalf", true), coordinate("alpha-second", "row", 2)] },
      { name: "second group keeps original field interactive", terminalInput: "beta\r", browserActions: [{ type: "fill", selector: input, value: "beta" }, { type: "press", selector: input, key: "Enter" }], comparisons: [callbackValue("both layers", "layer-count", 2), location("alpha-first", "leftHalf", true), location("beta-first", "rightHalf", true), coordinate("beta-second", "row", 2)] },
      { name: "hide both preserves underlying field", terminalInput: "clear\r", browserActions: [{ type: "fill", selector: input, value: "clear" }, { type: "press", selector: input, key: "Enter" }], comparisons: [callbackValue("all hidden", "layer-count", 0)] },
      { name: "field receives later keys", terminalInput: "usable", browserActions: [{ type: "fill", selector: input, value: "usable" }], comparisons: [{ name: "underlying input", terminal: { type: "screen", pattern: "(usable)", group: 1 }, browser: { type: "dom", selector: input, read: "value" }, expected: "usable" }] },
    ],
  },
  {
    id: "overlay-real-modal-head-cancel",
    covers: [component("components/input", "Input"), component("overlays/plan-save-overlay", "PlanSaveOverlay")],
    async create(context) {
      const [{ Input }, { PlanSaveOverlay }] = await Promise.all([
        context.load<typeof InputModule>("packages/tui/src/components/input.ts"),
        context.load<typeof PlanSaveModule>("packages/tui/src/overlays/plan-save-overlay.ts"),
      ]);
      const field = new Input();
      field.prompt = "Destination: ";
      field.onSubmit = () => {
        const dialog = new PlanSaveOverlay("scratch-plan.md", result => {
          mounted.hide();
          context.record("dialog-result", result?.path ?? "cancelled");
          field.setValue("");
          context.tui.requestRender();
        });
        const mounted = context.tui.showOverlay(dialog, { anchor: "center", width: "60%", margin: 1 });
        context.tui.requestRender();
      };
      return field;
    },
    checkpoints: [
      { name: "open actual modal with canonical head", terminalInput: "\r", browserActions: [{ type: "press", selector: input, key: "Enter" }], comparisons: [{ name: "real heading visible", terminal: { type: "screen", pattern: "(Save and quit)", group: 1 }, browser: { type: "dom", selector: '[role="dialog"] h3', read: "text" }, expected: "Save and quit" }] },
      { name: "modal owns typed input", terminalInput: "private-draft", browserActions: [{ type: "fill", selector: '[role="dialog"] input', value: "private-draft" }], comparisons: [{ name: "modal draft", terminal: { type: "screen", pattern: "(private-draft)", group: 1 }, browser: { type: "dom", selector: '[role="dialog"] input', read: "value" }, expected: "private-draft" }] },
      { name: "cancel returns to original input", terminalInput: "\x1b", browserActions: [{ type: "press", selector: '[role="dialog"] input', key: "Escape" }], comparisons: [callbackValue("actual cancellation", "dialog-result", "cancelled")] },
      { name: "underlying input restored", terminalInput: "restored", browserActions: [{ type: "fill", selector: input, value: "restored" }], comparisons: [{ name: "restored input draft", terminal: { type: "screen", pattern: "(restored)", group: 1 }, browser: { type: "dom", selector: input, read: "value" }, expected: "restored" }] },
    ],
  },
  {
    id: "overlay-real-editor-caret-collision",
    covers: [component("components/editor", "Editor")],
    async create(context) {
      const [{ Editor }, { getEditorTheme }, { CombinedAutocompleteProvider }] = await Promise.all([
        context.load<typeof EditorModule>("packages/tui/src/components/editor.ts"),
        context.load<typeof ThemeModule>("packages/tui/src/theme/tui-adapters.ts"),
        context.load<typeof AutocompleteModule>("packages/tui/src/autocomplete.ts"),
      ]);
      const editor = new Editor(getEditorTheme());
      editor.setMaxHeight(6);
      editor.setAutocompleteProvider(new CombinedAutocompleteProvider([{ name: "geoalpha", description: "First geometry command" }, { name: "geobeta", description: "Second geometry command" }], context.workspace));
      editor.onChange = value => context.record("draft", value);
      return editor;
    },
    checkpoints: [
      { name: "real slash autocomplete at caret", terminalInput: "/geo", browserActions: [{ type: "fill", selector: editorInput, value: "/geo" }], comparisons: [{ name: "actual completion choices", terminal: { type: "screen", pattern: "geoalpha", read: "visible" }, browser: { type: "dom", selector: `${autocomplete}:has-text("geoalpha")`, read: "insideViewport", relativeTo: surface }, expected: true }] },
      { name: "typed character moves real caret anchor", terminalInput: "a", browserActions: [{ type: "press", selector: editorInput, key: "a" }], comparisons: [{ name: "moving-caret completion stays visible", terminal: { type: "screen", pattern: "geoalpha", read: "visible" }, browser: { type: "dom", selector: `${autocomplete}:has-text("geoalpha")`, read: "insideViewport", relativeTo: surface }, expected: true }, callbackValue("typed caret draft", "draft", "/geoa")] },
      { name: "backspace moves caret without losing completion", terminalInput: "\x7f", browserActions: [{ type: "press", selector: editorInput, key: "Backspace" }], comparisons: [callbackValue("restored prefix", "draft", "/geo")] },
      { name: "phone viewport reflows existing caret popover", terminalResize: { columns: 40, rows: 18 }, browserActions: [{ type: "resize", width: 390, height: 480 }], comparisons: [{ name: "completion remains in visible pane", terminal: { type: "screen", pattern: "geobeta", read: "visible" }, browser: { type: "dom", selector: `${autocomplete}:has-text("geobeta")`, read: "insideViewport", relativeTo: surface }, expected: true }, { name: "draft survives resize", terminal: { type: "screen", pattern: "(/geo)", group: 1 }, browser: { type: "dom", selector: editorInput, read: "value" }, expected: "/geo" }] },
      { name: "height-only resize preserves same editor and popup", terminalResize: { columns: 40, rows: 12 }, browserActions: [{ type: "resize", width: 390, height: 360 }], comparisons: [{ name: "popup survives height-only collision", terminal: { type: "screen", pattern: "geoalpha", read: "visible" }, browser: { type: "dom", selector: `${autocomplete}:has-text("geoalpha")`, read: "insideViewport", relativeTo: surface }, expected: true }, { name: "draft survives height-only resize", terminal: { type: "screen", pattern: "(/geo)", group: 1 }, browser: { type: "dom", selector: editorInput, read: "value" }, expected: "/geo" }] },
      { name: "choose next real completion", terminalInput: "\x1b[B\t", browserActions: [{ type: "press", selector: editorInput, key: "ArrowDown" }, { type: "press", selector: editorInput, key: "Tab" }], comparisons: [callbackValue("accepted completion", "draft", "/geobeta ")] },
    ],
  },
];
