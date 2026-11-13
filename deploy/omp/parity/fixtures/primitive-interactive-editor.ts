import type * as EditorModule from "@omp-source/packages/tui/src/components/editor";
import type * as ThemeModule from "@omp-source/packages/tui/src/theme/tui-adapters";
import type { Comparison, FixtureCase } from "../scenario";

const component = "component:packages/tui/src/components/editor.ts#Editor";
const surface = '[id^="omp-view-"][data-omp-surface]';
const editorNode = '[data-omp-node="l.input"]';
const part = (name: string) => `${editorNode} [data-omp-composer-chrome-part="${name}"]`;
const border = (text: string) => ({ t: text, s: "borderMuted" });
const topRule = [[border(`╭${"─".repeat(104)}╮`)]];
const bodyRow = { prefix: [border("│  ")], suffix: [border("  │")] };
const closingRow = { prefix: [border("╰─ ")], suffix: [border(" ─╯")] };
const boxChrome = { top: topRule, only: closingRow, first: bodyRow, middle: bodyRow, last: closingRow, bottom: [] };
const lines = ["Chrome first line", "Chrome middle line", "Chrome last line"];

function leftEdge(name: string, terminalPattern: string, row: number, proves: Comparison["proves"]): Comparison {
  return {
    name, terminal: { type: "screen", pattern: terminalPattern, read: "leftHalf" },
    browser: { type: "dom", selector: `${part("prefix")} > div:nth-child(${row})`, read: "leftHalf", relativeTo: surface },
    expected: true, proves,
  };
}

async function createEditor(context: Parameters<FixtureCase["create"]>[0], text: string, style?: string) {
  const { Editor } = await context.load<typeof EditorModule>("packages/tui/src/components/editor.ts");
  const { getEditorTheme } = await context.load<typeof ThemeModule>("packages/tui/src/theme/tui-adapters.ts");
  const editor = new Editor(getEditorTheme());
  editor.setText(text);
  editor.setMaxHeight(6);
  if (style) editor.setBorderStyle(style as Parameters<typeof editor.setBorderStyle>[0]);
  return editor;
}

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-interactive-editor-box-chrome",
    covers: ["kind:editor", "property:TspEditorProps.chrome", "property:TspEditorChrome.top", "property:TspEditorChrome.first", "property:TspEditorChrome.middle", "property:TspEditorChrome.last", "property:TspEditorChromeRow.suffix", component],
    create: context => createEditor(context, lines.join("\n")),
    checkpoints: [{ name: "box frame rows", comparisons: [
      { name: "top rule sits in the composer area", terminal: { type: "screen", pattern: "╭─+╮", read: "bottomHalf" }, browser: { type: "dom", selector: part("top"), read: "bottomHalf", relativeTo: surface }, expected: true, proves: [{ entry: "property:TspEditorChrome.top", effect: "layout", value: topRule }, { entry: "property:TspEditorProps.chrome", effect: "layout", value: boxChrome }] },
      leftEdge("first row prefix", `│  (?=${lines[0]})`, 1, [{ entry: "property:TspEditorChrome.first", effect: "layout", value: bodyRow }]),
      leftEdge("middle row prefix", `│  (?=${lines[1]})`, 2, [{ entry: "property:TspEditorChrome.middle", effect: "layout", value: bodyRow }]),
      leftEdge("last row prefix", `╰─ (?=${lines[2]})`, 3, [{ entry: "property:TspEditorChrome.last", effect: "layout", value: closingRow }]),
      { name: "first row suffix closes the right edge", terminal: { type: "screen", pattern: `(?<=${lines[0]}[^\\n]*)│`, read: "rightHalf" }, browser: { type: "dom", selector: `${part("suffix")} > div:nth-child(1)`, read: "rightHalf", relativeTo: surface }, expected: true, proves: [{ entry: "property:TspEditorChromeRow.suffix", effect: "layout", value: bodyRow.suffix }] },
    ] }],
  },
  {
    id: "primitive-interactive-editor-single-row-chrome",
    covers: ["property:TspEditorChrome.only", "property:TspEditorProps.maxLines", component],
    create: context => createEditor(context, "Chrome only line"),
    checkpoints: [{ name: "single row frame", comparisons: [
      leftEdge("only row prefix", "╰─ (?=Chrome only line)", 1, [{ entry: "property:TspEditorChrome.only", effect: "layout", value: closingRow }]),
      { name: "input row stays in the lower half within its height cap", terminal: { type: "screen", pattern: "Chrome only line", read: "bottomHalf" }, browser: { type: "dom", selector: `${editorNode} [data-omp-native-editor]`, read: "bottomHalf", relativeTo: surface }, expected: true, proves: [{ entry: "property:TspEditorProps.maxLines", effect: "layout", value: 6 }] },
    ] }],
  },
  {
    id: "primitive-editor-claude-bottom-rule",
    covers: ["property:TspEditorChrome.bottom", component],
    create: context => createEditor(context, "Claude rule line", "claude"),
    checkpoints: [{ name: "bottom rule", comparisons: [
      { name: "bottom rule closes the input", terminal: { type: "screen", pattern: "(?<=Claude rule line[^\\n]*\\n[^\\n]*)─{20,}", read: "below", relativeTo: { pattern: "Claude rule line" } }, browser: { type: "dom", selector: part("bottom"), read: "below", relativeTo: `${editorNode} textarea` }, expected: true, proves: [{ entry: "property:TspEditorChrome.bottom", effect: "layout", value: [[border("─".repeat(104))]] }] },
    ] }],
  },
  {
    id: "primitive-editor-band-input-style",
    covers: ["property:TspEditorChromeRow.s", component],
    create: context => createEditor(context, "Band style line", "band"),
    checkpoints: [{ name: "editable cell paint", comparisons: [
      { name: "editable cell foreground", terminal: { type: "screen", pattern: "Band style line", read: "foreground" }, browser: { type: "dom", selector: `${editorNode} textarea`, read: "css", property: "color" }, expected: "x", proves: [{ entry: "property:TspEditorChromeRow.s", effect: "appearance", value: "text" }] },
    ] }],
  },
];
