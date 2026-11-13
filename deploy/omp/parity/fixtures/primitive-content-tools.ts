import type * as ToolExecutionModule from "@omp-source/packages/tui/src/chat/tool-execution";
import type * as ToolRenderers from "@omp-source/packages/tui/src/tools/index";
import type * as Describe from "@omp-source/packages/tui/src/native/describe";
import type * as GalleryFixtures from "@omp-source/packages/coding-agent/src/cli/gallery-fixtures/index";
import type { Comparison, FixtureCase, FixtureContext } from "../scenario";
import { contentComparison, controlledGallery } from "./gallery-support";

type ToolState = "running" | "success" | "error" | "unfinished";

interface ToolMount { key: string; state: ToolState; compactDiff?: string; displayContent?: string }

async function mountToolStack(context: FixtureContext, mounts: readonly ToolMount[]) {
  const { ToolExecutionComponent } = await context.load<typeof ToolExecutionModule>("packages/tui/src/chat/tool-execution.ts");
  const { toolRenderers } = await context.load<typeof ToolRenderers>("packages/tui/src/tools/index.ts");
  const { galleryFixtures } = await context.load<typeof GalleryFixtures>("packages/coding-agent/src/cli/gallery-fixtures/index.ts");
  const { col } = await context.load<typeof Describe>("packages/tui/src/native/describe.ts");
  const components = mounts.map(({ key, state, compactDiff, displayContent }) => {
    const base = galleryFixtures[key];
    const fixture = displayContent && base ? { ...base, result: { ...base.result, details: { ...(base.result.details as object), displayContent } } } : compactDiff && base ? { ...base, args: { ...(base.args as object), previewDiff: compactDiff }, result: { ...base.result, details: { ...(base.result.details as object), diff: compactDiff } } } : base;
    const renderer = toolRenderers[key];
    if (!fixture || fixture.renderState || !renderer?.renderCall) throw new Error(`Missing ToolExecutionComponent fixture: ${key}`);
    const component = new ToolExecutionComponent(key, fixture.args, { showImages: false, useBuiltInRenderer: !fixture.customRendered },
      fixture.editMode ? { name: key, label: fixture.label ?? key, mode: fixture.editMode } as never : undefined, context.tui, context.workspace);
    component.setExpanded(true);
    component.setArgsComplete();
    if (state === "unfinished") return component;
    component.setExecutionStarted();
    if (state === "running") return component;
    const result = state === "error" ? fixture.errorResult ?? { content: [{ type: "text", text: `Parity ${key} failure` }], isError: true } : fixture.result;
    component.updateResult({ content: result.content.flatMap(block => block.type === "text" && block.text !== undefined ? [{ type: "text" as const, text: block.text }] : []), details: result.details, isError: state === "error" }, false);
    return component;
  });
  await Promise.all(components.map(component => component.whenPreviewSettled(50)));
  const mounted = await controlledGallery(context, { render: (width: number) => components.flatMap(component => [...component.render(width)]), invalidate() {}, describe: () => col(components) } as never, {});
  const dispose = mounted.dispose;
  mounted.dispose = () => { for (const component of components) component.stopAnimation(); dispose?.(); };
  return mounted;
}

const gappedGrepDisplay = [
  "# packages/tui/src/components/",
  "## SearchBox.tsx",
  '*18│  const [query, setQuery] = useState("");',
  " 19│  const [open, setOpen] = useState(false);",
  "*42│  const [hits, setHits] = useState<Match[]>([]);",
  "*43│  const [cursor, setCursor] = useState(0);",
].join("\n");

const compactHunkLines = [" \tconst offset = args.offset ?? 1;", "-\tconst limit = args.limit ?? 2000;", "+\tconst limit = args.limit ?? 4000;", " \tconst raw = await Bun.file(path).text();"];
const astEditDiffText = [
  "@@ -468 +468 @@", "-\t\tfileCount = countEditFiles(editArgs.edits);", "+\t\tfileCount = countDistinctFiles(editArgs.edits);",
  "@@ -488 +488 @@", "-\t\tconst totalFiles = args?.edits ? countEditFiles(args.edits) : 0;", "+\t\tconst totalFiles = args?.edits ? countDistinctFiles(args.edits) : 0;",
].join("\n");

function diffRowShare(name: string, text: string, gutter: string, node: string, proves: Comparison["proves"]): Comparison {
  return {
    name,
    terminal: { type: "screen", pattern: text, read: "sameRow", relativeTo: { pattern: gutter } },
    browser: { type: "dom", selector: `${node} div[data-kind="removed"] > span:last-child`, read: "sameRow", relativeTo: `${node} div[data-kind="removed"] > span:first-child` },
    expected: true,
    proves,
  };
}

const readCode = '[data-omp-node$=".c437"]';
const editDiff = '[data-omp-role="omp.tool.edit"] [data-omp-node$=".0"]';
const compactEditDiff = [" 87|\tconst offset = args.offset ?? 1;", "-88|\tconst limit = args.limit ?? 2000;", "+88|\tconst limit = args.limit ?? 4000;", " 89|\tconst raw = await Bun.file(path).text();"].join("\n");

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-tool-compact-edit-write-glob",
    covers: [
      "component:packages/tui/src/chat/tool-execution.ts#ToolExecutionComponent",
      "property:TspDiffHunk.oldStart", "property:TspDiffHunk.newStart", "property:TspDiffHunk.lines", "property:TspDiffProps.hunks", "property:TspDiffProps.text",
    ],
    create: context => mountToolStack(context, [{ key: "edit", state: "success", compactDiff: compactEditDiff }, { key: "write", state: "success" }, { key: "glob", state: "success" }, { key: "ast_edit", state: "success" }]),
    checkpoints: [{ name: "edit write glob ast_edit", comparisons: [
      contentComparison("compact edit source", "(const limit = args\\.limit \\?\\? 4000;)", '[data-omp-role="omp.tool.edit"]', "const limit = args.limit ?? 4000;"),
      diffRowShare("compact edit removed row", "args\\.limit \\?\\? 2000", "-88│", '[data-omp-node="n.0"]', [
        { entry: "property:TspDiffHunk.oldStart", effect: "layout", value: 87 },
        { entry: "property:TspDiffHunk.newStart", effect: "layout", value: 87 },
        { entry: "property:TspDiffHunk.lines", effect: "layout", value: compactHunkLines },
        { entry: "property:TspDiffProps.hunks", effect: "layout", value: [{ oldStart: 87, newStart: 87, lines: compactHunkLines }] },
      ]),
      diffRowShare("ast edit removed row", "countEditFiles\\(editArgs", "-468", '[data-omp-node="q.edit%2Frenderer.ts/0"]', [
        { entry: "property:TspDiffProps.text", effect: "layout", value: astEditDiffText },
      ]),
    ] }],
  },
  {
    id: "primitive-tool-code-ansi-output",
    covers: [
      "component:packages/tui/src/chat/tool-execution.ts#ToolExecutionComponent",
      "property:TspCodeProps.start", "property:TspCodeProps.numbers", "property:TspCodeProps.path", "property:TspCodeProps.wrap", "property:TspCodeProps.lang", "property:TspCodeProps.preview",
      "property:TspAnsiProps.follow", "property:TspAnsiProps.preview", "property:TspDiffProps.path",
    ],
    create: context => mountToolStack(context, [{ key: "edit", state: "success" }, { key: "read", state: "success" }, { key: "bash", state: "running" }, { key: "eval", state: "error" }]),
    checkpoints: [{ name: "edit read bash eval tool cards", comparisons: [
      contentComparison("edit changed source", "(const limit = args\\.limit \\?\\? 4000;)", '[data-omp-role="omp.tool.edit"]', "const limit = args.limit ?? 4000;"),
      contentComparison("read first line", "(export const globToolRenderer = \\{)", '[data-omp-role="omp.tool.read"]', "export const globToolRenderer = {"),
      { name: "read line numbers sit beside source", terminal: { type: "screen", pattern: "437 export", read: "sameRow", relativeTo: { pattern: "globToolRenderer = \\{" } }, browser: { type: "dom", selector: `${readCode} div[data-line="437"] > span:first-child`, read: "sameRow", relativeTo: `${readCode} div[data-line="437"] > span:nth-child(2)` }, expected: true, proves: [
        { entry: "property:TspCodeProps.numbers", effect: "layout", value: true },
        { entry: "property:TspCodeProps.wrap", effect: "layout", value: true },
      ] },
      { name: "read first line number", terminal: { type: "screen", pattern: "(437) export const", group: 1 }, browser: { type: "dom", selector: `${readCode} div[data-line="437"] > span:first-child`, read: "text", pattern: "(437)", group: 1 }, expected: "437", proves: [{ entry: "property:TspCodeProps.start", effect: "literal", value: 437 }] },
      { name: "read path label", terminal: { type: "screen", pattern: "Read (packages/coding-agent/src/tools/glob\\.ts):437", group: 1 }, browser: { type: "dom", selector: `${readCode} > div > div > span`, read: "text", pattern: "(packages/coding-agent/src/tools/glob\\.ts)", group: 1 }, expected: "packages/coding-agent/src/tools/glob.ts", proves: [{ entry: "property:TspCodeProps.path", effect: "literal", value: "packages/coding-agent/src/tools/glob.ts" }] },
      { name: "edit diff path label", terminal: { type: "screen", pattern: "Edit: \\S+ (packages/coding-agent/src/tools/read\\.ts)", group: 1 }, browser: { type: "dom", selector: `${editDiff} > div > div > span`, read: "text", pattern: "(packages/coding-agent/src/tools/read\\.ts)", group: 1 }, expected: "packages/coding-agent/src/tools/read.ts", proves: [{ entry: "property:TspDiffProps.path", effect: "literal", value: "packages/coding-agent/src/tools/read.ts" }] },
      { name: "bash command block", terminal: { type: "screen", pattern: "cd packages/coding-agent", read: "sameRow", relativeTo: { pattern: "git log --oneline" } }, browser: { type: "dom", selector: `[data-omp-node$="run/input"] div[data-line="1"] > span`, read: "sameRow", relativeTo: `[data-omp-node$="run/input"] div[data-line="1"]` }, expected: true, proves: [
        { entry: "property:TspCodeProps.lang", effect: "layout", value: "bash" },
        { entry: "property:TspCodeProps.wrap", effect: "layout", value: true },
        { entry: "property:TspCodeProps.preview", effect: "layout", value: { lines: 6 } },
      ] },
      { name: "eval input block", terminal: { type: "screen", pattern: "import", read: "sameRow", relativeTo: { pattern: "json" } }, browser: { type: "dom", selector: `[data-omp-node$="cell-0/input"] div[data-line="1"] > span`, read: "sameRow", relativeTo: `[data-omp-node$="cell-0/input"] div[data-line="1"]` }, expected: true, proves: [
        { entry: "property:TspCodeProps.lang", effect: "layout", value: "python" },
        { entry: "property:TspCodeProps.wrap", effect: "layout", value: false },
        { entry: "property:TspCodeProps.preview", effect: "layout", value: { lines: 6 } },
      ] },
      { name: "eval output block", terminal: { type: "screen", pattern: "Traceback", read: "sameRow", relativeTo: { pattern: "most recent call" } }, browser: { type: "dom", selector: `[data-omp-node$="cell-0/output"] pre > div:first-child > span`, read: "sameRow", relativeTo: `[data-omp-node$="cell-0/output"] pre > div:first-child` }, expected: true, proves: [
        { entry: "property:TspAnsiProps.follow", effect: "layout", value: true },
        { entry: "property:TspAnsiProps.preview", effect: "layout", value: { lines: 10 } },
      ] },
    ] }],
  },
  {
    id: "primitive-tool-grep-marked-gapped-code",
    covers: [
      "component:packages/tui/src/chat/tool-execution.ts#ToolExecutionComponent",
      "property:TspCodeProps.marks", "property:TspCodeProps.nums", "property:TspCodeProps.marks[].line", "property:TspCodeProps.marks[].tone",
    ],
    create: context => mountToolStack(context, [{ key: "grep", state: "success", displayContent: gappedGrepDisplay }]),
    checkpoints: [{ name: "grep gapped marked lines", comparisons: [
      contentComparison("grep first match", "(const \\[hits, setHits\\])", '[data-omp-role="omp.tool.grep"]', "const [hits, setHits]"),
    ] }],
  },
];
