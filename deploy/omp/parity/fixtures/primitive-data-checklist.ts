import type * as ToolExecutionModule from "@omp-source/packages/tui/src/chat/tool-execution";
import type * as Describe from "@omp-source/packages/tui/src/native/describe";
import type { Comparison, FixtureCase } from "../scenario";
import { contentComparison, controlledGallery } from "./gallery-support";
import { escapePattern, layoutClaims } from "./primitive-data-support";

const card = '[data-omp-role="omp.tool.todo"]';
const phasesDetail = [
  { name: "Setup", tasks: [{ content: "Scaffold crate", status: "completed" }, { content: "Wire workspace", status: "completed" }] },
  { name: "Build", tasks: [{ content: "Retire old path", status: "abandoned" }, { content: "Wire parser", status: "completed" }, { content: "Wire printer", status: "in_progress" }, { content: "Wire logger", status: "pending" }] },
  { name: "Ship", tasks: [{ content: "Drop shim", status: "abandoned" }, { content: "Run gate", status: "in_progress" }, { content: "Await review", status: "blocked", blocker: "Needs approval" }] },
];

const items = {
  setup: [{ id: "p0.0", text: "Scaffold crate", status: "done" }, { id: "p0.1", text: "Wire workspace", status: "done" }],
  build: [{ id: "p1.0", text: "Retire old path", status: "dropped" }, { id: "p1.1", text: "Wire parser", status: "done" }, { id: "p1.2", text: "Wire printer", status: "active" }, { id: "p1.3", text: "Wire logger", status: "pending" }],
  ship: [{ id: "p2.0", text: "Drop shim", status: "dropped" }, { id: "p2.1", text: "Run gate", status: "active" }, { id: "p2.2", text: "Await review", status: "blocked", note: "Needs approval" }],
};
const phases = [
  { id: "p0", title: "Setup", items: items.setup, collapsed: true },
  { id: "p1", title: "Build", items: items.build, collapsed: false },
  { id: "p2", title: "Ship", items: items.ship, collapsed: false },
];

const item = (text: string) => `${card} li:has(:text-is(${JSON.stringify(text)})) span.text-sm`;

function below(name: string, text: string, previous: string, proves: NonNullable<Comparison["proves"]>): Comparison {
  return {
    name,
    terminal: { type: "screen", pattern: escapePattern(text), relativeTo: { pattern: escapePattern(previous) }, read: "below" },
    browser: { type: "dom", selector: item(text), relativeTo: item(previous), read: "below" },
    expected: true,
    proves,
  };
}

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-checklist-todo-result",
    covers: [
      "kind:checklist",
      "property:TspChecklistProps.mode", "property:TspChecklistProps.phases",
      "property:TspChecklistPhase.id", "property:TspChecklistPhase.title", "property:TspChecklistPhase.items", "property:TspChecklistPhase.collapsed",
      "property:TspChecklistItem.id", "property:TspChecklistItem.status", "property:TspChecklistItem.note",
      "component:packages/tui/src/chat/tool-execution.ts#ToolExecutionComponent",
    ],
    async create(context) {
      const { ToolExecutionComponent } = await context.load<typeof ToolExecutionModule>("packages/tui/src/chat/tool-execution.ts");
      const { col } = await context.load<typeof Describe>("packages/tui/src/native/describe.ts");
      const component = new ToolExecutionComponent("todo", { op: "update" }, { showImages: false, useBuiltInRenderer: true }, undefined, context.tui, context.workspace);
      component.setArgsComplete();
      component.setExecutionStarted();
      component.updateResult({ content: [{ type: "text", text: "Todo list updated" }], details: { storage: "session", phases: phasesDetail }, isError: false } as never, false);
      await component.whenPreviewSettled(50);
      const mounted = await controlledGallery(context, { render: (width: number) => [...component.render(width)], invalidate() {}, describe: () => col([component]) } as never, {});
      const dispose = mounted.dispose;
      mounted.dispose = () => { component.stopAnimation(); dispose?.(); };
      return mounted;
    },
    checkpoints: [{ name: "todo result phases and statuses", comparisons: [
      {
        name: "settled phase is a collapsed summary",
        terminal: { type: "screen", pattern: "Setup", relativeTo: { pattern: "2/2" }, read: "sameRow" },
        browser: { type: "dom", selector: `${card} strong:has-text("Setup")`, relativeTo: `${card} :text-is("2/2")`, read: "sameRow" },
        expected: true,
        proves: [
          { entry: "property:TspChecklistPhase.collapsed", effect: "layout", value: true },
          { entry: "property:TspChecklistPhase.id", effect: "layout", value: "p0" },
          ...layoutClaims("TspChecklistProps", { mode: "full", phases }),
        ],
      },
      {
        name: "open phase shows its progress beside the title",
        terminal: { type: "screen", pattern: "Build", relativeTo: { pattern: "2/4" }, read: "sameRow" },
        browser: { type: "dom", selector: `${card} strong:text-is("Build")`, relativeTo: `${card} :text-is("2/4")`, read: "sameRow" },
        expected: true,
        proves: [{ entry: "property:TspChecklistPhase.collapsed", effect: "layout", value: false }, { entry: "property:TspChecklistPhase.items", effect: "layout", value: items.build }],
      },
      contentComparison("phase title", "\\. (Build)", `${card} strong:text-is("Build")`, "Build", [{ entry: "property:TspChecklistPhase.title", effect: "literal", value: "Build" }]),
      below("done item", "Wire parser", "Build", [{ entry: "property:TspChecklistItem.status", effect: "layout", value: "done" }, { entry: "property:TspChecklistItem.id", effect: "layout", value: "p1.1" }]),
      below("active item", "Wire printer", "Wire parser", [{ entry: "property:TspChecklistItem.status", effect: "layout", value: "active" }]),
      below("pending item", "Wire logger", "Wire printer", [{ entry: "property:TspChecklistItem.status", effect: "layout", value: "pending" }]),
      below("dropped item", "Drop shim", "Ship", [{ entry: "property:TspChecklistItem.status", effect: "layout", value: "dropped" }]),
      below("blocked item", "Await review", "Run gate", [{ entry: "property:TspChecklistItem.status", effect: "layout", value: "blocked" }]),
      contentComparison("blocked note", "blocked: (Needs approval)", `${item("Await review")} ~ p`, "Needs approval", [{ entry: "property:TspChecklistItem.note", effect: "literal", value: "Needs approval" }]),
    ] }],
  },
];
