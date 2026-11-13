import type * as TextModule from "@omp-source/packages/tui/src/components/text";
import type * as MarkdownModule from "@omp-source/packages/tui/src/components/markdown";
import type * as ThemeModule from "@omp-source/packages/tui/src/theme/tui-adapters";
import type * as BoxModule from "@omp-source/packages/tui/src/components/box";
import type * as RowModule from "@omp-source/packages/tui/src/components/layout/row";
import type * as SectionModule from "@omp-source/packages/tui/src/components/section";
import type * as SpacerModule from "@omp-source/packages/tui/src/components/spacer";
import type * as TableModule from "@omp-source/packages/tui/src/components/table";
import type * as KeyValueModule from "@omp-source/packages/tui/src/components/key-value-list";
import type * as TreeModule from "@omp-source/packages/tui/src/components/tree-view";
import type * as DividerModule from "@omp-source/packages/tui/src/chrome/message-divider";
import type * as NoticeModule from "@omp-source/packages/tui/src/chrome/status-notice";
import { callbackValue, type FixtureCase } from "../scenario";
import { fixtureCases as loaderCases } from "./primitive-content-loader";
import { contentComparison, controlledGallery, linkComparison } from "./gallery-support";

const component = (file: string, name: string) => `component:packages/tui/src/${file}.ts#${name}`;
const exact = (text: string) => `text=${JSON.stringify(text)}`;

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-styled-unicode-text",
    covers: ["kind:text", "property:TspTextProps.spans", "property:TspSpan.t", "property:TspSpan.s", "property:TspSpan.href", component("components/text", "Text")],
    async create(context) {
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      return new Text("\x1b[1mBold café\x1b[22m · \x1b[3mItalic 日本語\x1b[23m\n\x1b]8;;https://example.invalid/parity\x07Linked 🧑🏽‍💻\x1b]8;;\x07", 0, 0);
    },
    checkpoints: [{ name: "styled runs retain Unicode and safe links", comparisons: [
      contentComparison("bold text", "(Bold café)", ".font-semibold:text-is(\"Bold café\")", "Bold café", [{ entry: "property:TspSpan.t", effect: "literal", value: "Bold café" }]),
      contentComparison("italic text", "(Italic 日本語)", ".italic:text-is(\"Italic 日本語\")", "Italic 日本語", [{ entry: "property:TspSpan.t", effect: "literal", value: "Italic 日本語" }]),
      contentComparison("linked grapheme", "(Linked 🧑🏽‍💻)", 'a[href="https://example.invalid/parity"]', "Linked 🧑🏽‍💻", [{ entry: "property:TspSpan.t", effect: "literal", value: "Linked 🧑🏽‍💻" }]),
      { name: "bold cells match computed font weight", terminal: { type: "screen", pattern: "(Bold café)", group: 1, read: "bold" }, browser: { type: "dom", selector: '.font-semibold:text-is("Bold café")', read: "bold" }, expected: true, proves: [{ entry: "property:TspSpan.s", effect: "appearance", value: "strong" }, { entry: "property:TspTextProps.spans", effect: "appearance", value: [{ t: "Bold café", s: "strong" }, { t: " · " }, { t: "Italic 日本語", s: "em" }, { t: "\n" }, { t: "Linked 🧑🏽‍💻", href: "https://example.invalid/parity" }] }] },
      { name: "italic cells match computed font style", terminal: { type: "screen", pattern: "(Italic 日本語)", group: 1, read: "italic" }, browser: { type: "dom", selector: '.italic:text-is("Italic 日本語")', read: "italic" }, expected: true, proves: [{ entry: "property:TspSpan.s", effect: "appearance", value: "em" }] },
      linkComparison("emitted grapheme hyperlink", "Linked 🧑🏽‍💻", "https://example.invalid/parity", 'a[href="https://example.invalid/parity"]', { checkpoint: "styled runs retain Unicode and safe links", comparison: "linked grapheme" }, "property:TspSpan.href"),
    ] }],
  },
  {
    id: "primitive-soft-word-wrap",
    covers: ["kind:text", "property:TspTextProps.wrap", component("components/text", "Text")],
    presentation: { overlay: true, overlayOptions: { width: 24, minWidth: 24 } },
    async create(context) {
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      return new Text(`\x1b[1mSoft wrap start\x1b[22m ${"canonical wrapping words ".repeat(12)}\x1b[3mSoft wrap end\x1b[23m`, 0, 0);
    },
    checkpoints: [{ name: "long single source line wraps into visible rows", comparisons: [
      contentComparison("soft wrap start marker", "(Soft wrap start)", '.font-semibold:text-is("Soft wrap start")', "Soft wrap start"),
      contentComparison("soft wrap end marker", "(Soft wrap end)", '.italic:text-is("Soft wrap end")', "Soft wrap end"),
      { name: "word wrapping creates actual vertical separation", terminal: { type: "screen", pattern: "Soft wrap end", relativeTo: { pattern: "Soft wrap start" }, read: "below" }, browser: { type: "dom", selector: '.italic:text-is("Soft wrap end")', relativeTo: '.font-semibold:text-is("Soft wrap start")', read: "below" }, expected: true, proves: [{ entry: "property:TspTextProps.wrap", effect: "layout", value: "word" }] },
    ] }],
  },
  {
    id: "primitive-markdown-content",
    covers: ["kind:md", "property:TspMarkdownProps.text", component("components/markdown", "Markdown")],
    async create(context) {
      const { Markdown } = await context.load<typeof MarkdownModule>("packages/tui/src/components/markdown.ts");
      const { getMarkdownTheme } = await context.load<typeof ThemeModule>("packages/tui/src/theme/tui-adapters.ts");
      return new Markdown("# Native markdown\n\n**Strong prose** with [reference](https://example.invalid/markdown).\n\n- First list entry\n- Unicode é 中\n\n```typescript\nconst total = 42;\n```", 0, 0, getMarkdownTheme());
    },
    checkpoints: [{ name: "markdown semantic content", comparisons: [
      contentComparison("heading", "(Native markdown)", "h1", "Native markdown"),
      contentComparison("strong prose", "(Strong prose)", "strong", "Strong prose"),
      contentComparison("link", "(reference)", 'a[href="https://example.invalid/markdown"]', "reference"),
      contentComparison("list entry", "(First list entry)", 'li:has-text("First list entry")', "First list entry"),
      contentComparison("code body", "(const total = 42;)", 'pre:has-text("const total = 42;")', "const total = 42;"),
    ] }],
  },
  {
    id: "primitive-production-layout",
    covers: ["kind:col", "kind:row", "kind:card", "kind:section", "kind:spacer", "property:TspSectionProps.head", "property:TspCardProps.inset", "property:TspRowProps.align", "property:TspSpacerProps.size", component("components/box", "Box"), component("components/layout/row", "Row"), component("components/section", "Section"), component("components/spacer", "Spacer")],
    async create(context) {
      const { Box } = await context.load<typeof BoxModule>("packages/tui/src/components/box.ts");
      const { Row } = await context.load<typeof RowModule>("packages/tui/src/components/layout/row.ts");
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      const { Section } = await context.load<typeof SectionModule>("packages/tui/src/components/section.ts");
      const { Spacer } = await context.load<typeof SpacerModule>("packages/tui/src/components/spacer.ts");
      const root = new Box(0, 0);
      const card = new Box(1, 1, text => context.theme.bg("userMessageBg", text));
      card.addChild(new Row({ children: [{ content: new Text("Left column", 0, 0), width: 20 }, { content: new Text("Right column\nSecond right line\nThird right line", 0, 0), grow: 1 }], align: "start" }));
      root.addChild(new Section({ title: "Structured section", body: card, ruleGlyph: "─", ruleWidth: "fill" }));
      root.addChild(new Spacer(1));
      root.addChild(new Text("After spacing", 0, 0));
      return root;
    },
    checkpoints: [{ name: "nested native layout retains content order", comparisons: [
      contentComparison("section heading", "(Structured section)", exact("Structured section"), "Structured section", [{ entry: "property:TspSectionProps.head", effect: "literal", value: "Structured section" }]),
      contentComparison("first column", "(Left column)", '[data-omp-role="omp.user"]', "Left column"),
      contentComparison("second column", "(Right column)", '[data-omp-role="omp.user"]', "Right column"),
      contentComparison("content after spacer", "(After spacing)", exact("After spacing"), "After spacing"),
      contentComparison("row body owner", "(Left column[\\s\\S]*Third right line)", '[data-omp-role="omp.user"] [data-omp-node]:has-text("Left column"):has-text("Third right line")', "Left column Right column Second right line Third right line"),
      { name: "unequal height columns align their first rows", terminal: { type: "screen", pattern: "Left column", relativeTo: { pattern: "Right column" }, read: "sameRow" }, browser: { type: "dom", selector: exact("Left column"), relativeTo: '[data-omp-role="omp.user"] span:has-text("Right column"):has-text("Third right line")', read: "sameRow" }, expected: true, proves: [{ entry: "property:TspRowProps.align", effect: "layout", value: "start", target: { checkpoint: "nested native layout retains content order", comparison: "row body owner" } }] },
    ] }],
  },
  {
    id: "primitive-spacer-height-transition",
    covers: ["kind:spacer", "property:TspSpacerProps.size", component("components/spacer", "Spacer"), component("components/box", "Box")],
    async create(context) {
      const [{ Box }, { Text }, { Spacer }] = await Promise.all([
        context.load<typeof BoxModule>("packages/tui/src/components/box.ts"),
        context.load<typeof TextModule>("packages/tui/src/components/text.ts"),
        context.load<typeof SpacerModule>("packages/tui/src/components/spacer.ts"),
      ]);
      const root = new Box(0, 0);
      const spacer = new Spacer(1);
      root.addChild(new Text("Before measured gap", 0, 0));
      root.addChild(spacer);
      root.addChild(new Text("After measured gap", 0, 0));
      return controlledGallery(context, root, { "m medium spacing": () => { spacer.setLines(2); context.record("spacer-height", spacer.debugState().height); } });
    },
    checkpoints: [
      { name: "small spacing has real blank extent", comparisons: [
        { name: "small spacer rendered extent", terminal: { type: "screen", pattern: "After measured gap", relativeTo: { pattern: "Before measured gap" }, read: "rowGap" }, browser: { type: "dom", selector: '[data-omp-node] > .h-2.w-2', read: "rectHeight" }, temporal: { direction: "sample" } },
        { name: "small spacer separates real adjacent labels", terminal: { type: "screen", pattern: "After measured gap", relativeTo: { pattern: "Before measured gap" }, read: "below" }, browser: { type: "dom", selector: exact("After measured gap"), relativeTo: exact("Before measured gap"), read: "below" }, expected: true, proves: [{ entry: "property:TspSpacerProps.size", effect: "layout", value: "sm", target: { checkpoint: "small spacing has real blank extent", comparison: "small spacer rendered extent" } }] },
      ] },
      { name: "production setter grows spacing on both renderers", terminalInput: "m", browserActions: [{ type: "click", selector: 'button:has-text("m medium spacing")' }], comparisons: [
        callbackValue("source spacer line count changed", "spacer-height", 2),
        { name: "medium spacer rendered extent grows independently", terminal: { type: "screen", pattern: "After measured gap", relativeTo: { pattern: "Before measured gap" }, read: "rowGap" }, browser: { type: "dom", selector: '[data-omp-node] > .h-4.w-4', read: "rectHeight" }, temporal: { direction: "increase", from: { checkpoint: "small spacing has real blank extent", comparison: "small spacer rendered extent" } }, proves: [{ entry: "property:TspSpacerProps.size", effect: "layout", value: "md", before: { checkpoint: "small spacing has real blank extent", comparison: "small spacer separates real adjacent labels" } }] },
      ] },
    ],
  },
  {
    id: "primitive-table-key-values-tree",
    covers: ["kind:table", "kind:kv", "kind:tree", "property:TspTableProps.cols", "property:TspTableProps.rows", "property:TspTableColumn.align", "property:TspKvProps.items", "property:TspKvProps.layout", "property:TspTreeProps.nodes", "property:TspTreeNode.children", "property:TspSpan.t", "property:TspSpan.s", "property:TspSpan.href", component("components/table", "Table"), component("components/key-value-list", "KeyValueList"), component("components/tree-view", "TreeView")],
    async create(context) {
      const { Box } = await context.load<typeof BoxModule>("packages/tui/src/components/box.ts");
      const { Table } = await context.load<typeof TableModule>("packages/tui/src/components/table.ts");
      const { KeyValueList } = await context.load<typeof KeyValueModule>("packages/tui/src/components/key-value-list.ts");
      const { TreeView } = await context.load<typeof TreeModule>("packages/tui/src/components/tree-view.ts");
      const root = new Box(0, 0);
      root.addChild(new Table([[{ text: "Widget", style: text => `\x1b[1m${text}\x1b[22m` }, { text: "42" }], [{ text: "Gadget", style: text => `\x1b]8;;https://example.invalid/table\x07${text}\x1b]8;;\x07` }, { text: "7" }]], [{ width: 16, align: "left", overflow: "truncate" }, { width: 6, align: "right", overflow: "allow" }]));
      root.addChild(new KeyValueList([{ label: "Owner", value: "Café team", labelStyle: text => `\x1b[3m${text}\x1b[23m`, valueStyle: text => `\x1b]8;;https://example.invalid/owner\x07${text}\x1b]8;;\x07` }, { label: "State", value: "Ready 日本語" }], { labelWidth: 8 }));
      interface Entry { id: string; title: string; children: Entry[] }
      const roots: Entry[] = [{ id: "root", title: "Project root", children: [{ id: "child", title: "\x1b]8;;https://example.invalid/tree\x07\x1b[1mNested source\x1b[22m\x1b]8;;\x07", children: [] }] }];
      root.addChild(new TreeView({ roots, theme: context.theme, getKey: entry => entry.id, getChildren: entry => entry.children, renderRow: entry => entry.title }));
      return root;
    },
    checkpoints: [{ name: "structured data source values", comparisons: [
      contentComparison("first table row", "(Widget)", "tbody > tr:nth-child(1) > td:nth-child(1)", "Widget", [{ entry: "property:TspSpan.t", effect: "literal", value: "Widget" }]),
      contentComparison("right aligned numeric cell", "(42)", "tbody > tr:nth-child(1) > td:nth-child(2)", "42"),
      contentComparison("key value", "(Café team)", 'dd:has-text("Café team")', "Café team", [{ entry: "property:TspSpan.t", effect: "literal", value: "Café team" }]),
      contentComparison("nested tree child", "(Nested source)", 'a[href="https://example.invalid/tree"]', "Nested source", [{ entry: "property:TspSpan.t", effect: "literal", value: "Nested source" }]),
      contentComparison("styled table cell", "(Widget)", 'td .font-semibold:text-is("Widget")', "Widget"),
      contentComparison("linked table cell", "(Gadget)", 'td a[href="https://example.invalid/table"]', "Gadget"),
      contentComparison("styled key label", "(Owner)", 'dt .italic:text-is("Owner")', "Owner"),
      contentComparison("linked key value", "(Café team)", 'dd a[href="https://example.invalid/owner"]', "Café team"),
      contentComparison("styled tree link", "(Nested source)", 'a[href="https://example.invalid/tree"].font-semibold, a[href="https://example.invalid/tree"] .font-semibold', "Nested source"),
      { name: "table bold cells", terminal: { type: "screen", pattern: "(Widget)", group: 1, read: "bold" }, browser: { type: "dom", selector: 'td .font-semibold:text-is("Widget")', read: "bold" }, expected: true, proves: [{ entry: "property:TspSpan.s", effect: "appearance", value: "strong" }] },
      { name: "key label italic cells", terminal: { type: "screen", pattern: "(Owner)", group: 1, read: "italic" }, browser: { type: "dom", selector: 'dt .italic:text-is("Owner")', read: "italic" }, expected: true, proves: [{ entry: "property:TspSpan.s", effect: "appearance", value: "em" }] },
      { name: "tree bold cells", terminal: { type: "screen", pattern: "(Nested source)", group: 1, read: "bold" }, browser: { type: "dom", selector: 'a[href="https://example.invalid/tree"].font-semibold, a[href="https://example.invalid/tree"] .font-semibold', read: "bold" }, expected: true, proves: [{ entry: "property:TspSpan.s", effect: "appearance", value: "strong" }] },
      linkComparison("emitted table hyperlink", "Gadget", "https://example.invalid/table", 'td a[href="https://example.invalid/table"]', { checkpoint: "structured data source values", comparison: "linked table cell" }, "property:TspSpan.href"),
      linkComparison("emitted key value hyperlink", "Café team", "https://example.invalid/owner", 'dd a[href="https://example.invalid/owner"]', { checkpoint: "structured data source values", comparison: "linked key value" }, "property:TspSpan.href"),
      linkComparison("emitted tree hyperlink", "Nested source", "https://example.invalid/tree", 'a[href="https://example.invalid/tree"]', { checkpoint: "structured data source values", comparison: "nested tree child" }, "property:TspSpan.href"),
      { name: "grid label and value share a real rendered row", terminal: { type: "screen", pattern: "Owner", relativeTo: { pattern: "Café team" }, read: "sameRow" }, browser: { type: "dom", selector: 'dt:has-text("Owner")', relativeTo: 'dd:has-text("Café team")', read: "sameRow" }, expected: true, proves: [{ entry: "property:TspKvProps.layout", effect: "layout", value: "grid" }, { entry: "property:TspKvProps.items", effect: "layout", value: [{"k":[{"t":"Owner","s":"em"}],"v":[{"t":"Café team","href":"https://example.invalid/owner"}]},{"k":"State","v":"Ready 日本語"}] }] },
      { name: "table cells of one row share a rendered row", terminal: { type: "screen", pattern: "Widget", relativeTo: { pattern: "42" }, read: "sameRow" }, browser: { type: "dom", selector: "tbody > tr:nth-child(1) > td:nth-child(1)", relativeTo: "tbody > tr:nth-child(1) > td:nth-child(2)", read: "sameRow" }, expected: true, proves: [{ entry: "property:TspTableProps.cols", effect: "layout", value: [{"id":"c0","align":"start","truncate":"end","priority":0},{"id":"c1","align":"end","priority":0}] }, { entry: "property:TspTableProps.rows", effect: "layout", value: [{"id":"r0","cells":{"c0":[{"t":"Widget","s":"strong"}],"c1":"42"}},{"id":"r1","cells":{"c0":[{"t":"Gadget","href":"https://example.invalid/table"}],"c1":"7"}}] }, { entry: "property:TspTableColumn.align", effect: "layout", value: "start" }, { entry: "property:TspTableColumn.align", effect: "layout", value: "end" }] },
      { name: "nested tree node renders below its parent", terminal: { type: "screen", pattern: "Nested source", relativeTo: { pattern: "Project root" }, read: "below" }, browser: { type: "dom", selector: 'a[href="https://example.invalid/tree"]', relativeTo: 'button[data-name="CollapsibleTrigger"]:has-text("Project root")', read: "below" }, expected: true, proves: [{ entry: "property:TspTreeProps.nodes", effect: "layout", value: [{"id":"root","label":[{"t":"Project root","s":"mark"}],"children":[{"id":"child","label":[{"t":"Nested source","s":"strong","href":"https://example.invalid/tree"}]}],"open":true}] }, { entry: "property:TspTreeNode.children", effect: "layout", value: [{"id":"child","label":[{"t":"Nested source","s":"strong","href":"https://example.invalid/tree"}]}] }] },
    ] }],
  },
  {
    id: "primitive-labelled-rule",
    covers: ["kind:rule", "property:TspRuleProps.label", "property:TspSpan.t", component("chrome/message-divider", "MessageDividerComponent")],
    async create(context) {
      const { MessageDividerComponent } = await context.load<typeof DividerModule>("packages/tui/src/chrome/message-divider.ts");
      return new MessageDividerComponent({ label: () => "Checkpoint boundary", labelColor: "dim" });
    },
    checkpoints: [{ name: "rule has real label", comparisons: [contentComparison("divider label", "(Checkpoint boundary)", '[data-omp-role="omp.divider"]', "Checkpoint boundary", [{ entry: "property:TspSpan.t", effect: "literal", value: "Checkpoint boundary" }]),
      { name: "divider label leads the rule", terminal: { type: "screen", pattern: "Checkpoint boundary", read: "sameRow", relativeTo: { pattern: "─{10}" } }, browser: { type: "dom", selector: '[data-omp-role="omp.divider"] span', read: "sameRow", relativeTo: '[data-omp-role="omp.divider"] hr' }, expected: true, proves: [{ entry: "property:TspRuleProps.label", effect: "layout", value: [{ t: "Checkpoint boundary", s: "dim" }] }] },
    ] }],
  },
  {
    id: "primitive-status-toast",
    covers: ["kind:toast", "property:TspToastProps.text", component("chrome/status-notice", "StatusNotice")],
    async create(context) {
      const { StatusNotice } = await context.load<typeof NoticeModule>("packages/tui/src/chrome/status-notice.ts");
      return new StatusNotice("Parity clipboard copied");
    },
    checkpoints: [{ name: "transient status notice", comparisons: [contentComparison("status message", "(Parity clipboard copied)", '[data-omp-role="omp.toast.status"]', "Parity clipboard copied", [{ entry: "property:TspToastProps.text", effect: "literal", value: "Parity clipboard copied" }])] }],
  },
  ...loaderCases,
];
