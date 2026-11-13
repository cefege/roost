import type * as TodoReminderModule from "@omp-source/packages/tui/src/chat/todo-reminder";
import type { TodoItem } from "@omp-source/packages/tui/src/tools/todo";
import type { Comparison, FixtureCase } from "../scenario";
import { contentComparison } from "./gallery-support";

const reminderRole = '[data-omp-role="omp.notice.todo"]';
const todoItems: readonly { content: string; status: TodoItem["status"]; checklist: string }[] = [
  { content: "Todo pending item", status: "pending", checklist: "pending" },
  { content: "Todo active item", status: "in_progress", checklist: "active" },
  { content: "Todo blocked item", status: "blocked", checklist: "blocked" },
];

const itemSelector = (content: string) => `${reminderRole} :text-is(${JSON.stringify(content)})`;

function itemRowComparison(index: number): Comparison {
  const item = todoItems[index]!;
  const previous = todoItems[index - 1]!;
  return {
    name: `${item.checklist} item sits below the previous item`,
    terminal: { type: "screen", pattern: item.content, relativeTo: { pattern: previous.content }, read: "below" },
    browser: { type: "dom", selector: itemSelector(item.content), relativeTo: itemSelector(previous.content), read: "below" },
    expected: true,
    proves: [{ entry: "property:TspChecklistItem.status", effect: "layout", value: item.checklist }],
  };
}

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-checklist-reminder",
    covers: ["kind:checklist", "property:TspChecklistProps.mode", "property:TspChecklistProps.note", "property:TspChecklistItem.status", "component:packages/tui/src/chat/todo-reminder.ts#TodoReminderComponent"],
    async create(context) {
      const { TodoReminderComponent } = await context.load<typeof TodoReminderModule>("packages/tui/src/chat/todo-reminder.ts");
      const todos: TodoItem[] = todoItems.map(({ content, status }) => ({ content, status }));
      return new TodoReminderComponent(todos, 1, 3);
    },
    checkpoints: [{ name: "reminder lists the incomplete items", comparisons: [
      contentComparison("first item", "(Todo pending item)", itemSelector("Todo pending item"), "Todo pending item"),
      contentComparison("reminder note", "(reminder 1/3)", `${reminderRole} :text-is("reminder 1/3")`, "reminder 1/3", [{ entry: "property:TspChecklistProps.note", effect: "literal", value: "reminder 1/3" }]),
      { name: "pending item sits below the reminder header", terminal: { type: "screen", pattern: "Todo pending item", relativeTo: { pattern: "reminder 1/3" }, read: "below" }, browser: { type: "dom", selector: itemSelector("Todo pending item"), relativeTo: `${reminderRole} :text-matches("reminder 1/3")`, read: "below" }, expected: true, proves: [{ entry: "property:TspChecklistItem.status", effect: "layout", value: "pending" }, { entry: "property:TspChecklistProps.mode", effect: "layout", value: "reminder" }] },
      ...todoItems.slice(1).map((_, offset) => itemRowComparison(offset + 1)),
    ] }],
  },
];
