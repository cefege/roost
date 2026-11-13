import type { BuiltinCase } from "./scenario";
import type { BuiltinProviderTurn } from "./builtin-requirements";
import { composer, effect } from "./builtin-steps";
import { chatCase, content, literalContent, markdownContent, settled, toolComponent } from "./chat-steps";

const todoHud = '[data-omp-role="omp.hud.todo"]';
const todoTask = `${todoHud} li span:not(:has(span)):text-is("Parity actionable task")`;
const anchoredTodo = `${todoHud}:has-text("TODO"):has-text("Parity actionable task")`;
const todoContainer = "component:packages/coding-agent/src/modes/interactive-mode.ts#TodoHudContainer";

export const builtinCases: BuiltinCase[] = [{
  ...chatCase("todo-lifecycle", "Parity initialize the scratch todo plan with one actionable task.", [
    {
      name: "Actual model tool result updates the real checklist HUD",
      terminalResize: { columns: 120, rows: 48 },
      browserActions: [{ type: "resize", width: 1200, height: 960 }, { type: "click", selector: todoHud }],
      comparisons: [
        markdownContent("PARITY_TODO_STARTED"),
        literalContent("TODO", anchoredTodo, "property:TspSpan.t"),
        literalContent("Parity actionable task", todoTask, "property:TspChecklistItem.text"),
        effect("builtin.todos", "in_progress", "/0/tasks/0/status"), effect("builtin.session", true, "/isStreaming"),
      ],
    },
    {
      name: "Actual normal composer steers completion of the retained todo",
      terminalInput: "Parity complete the scratch todo task.\r",
      browserActions: [{ type: "click", selector: '[data-omp-role="omp.hud.todo"]' }, { type: "fill", selector: composer, value: "Parity complete the scratch todo task." }, { type: "press", selector: composer, key: "Enter" }],
      comparisons: [content("PARITY_TODO_STARTED"), effect("builtin.session", ["Parity complete the scratch todo task."], "/steeringQueue"), effect("builtin.todos", "in_progress", "/0/tasks/0/status")],
    },
    {
      ...settled("PARITY_TODO_COMPLETED", [effect("builtin.todos", "completed", "/0/tasks/0/status"), effect("builtin.session", [], "/steeringQueue"), effect("builtin.provider", 4, "/requestCount")]),
      releaseProviderTurns: [1],
    },
  ], [toolComponent, todoContainer, "tool:todo", "kind:checklist", "property:TspSpan.t", "property:TspChecklistItem.text"]),
  settings: { "todo.enabled": true, "todo.reminders": false, "tools.approvalMode": "yolo" },
}];

const interactiveMode = "component:packages/coding-agent/src/modes/interactive-mode.ts";
const compactCovers = [`${interactiveMode}#InteractiveMode`, `${interactiveMode}#StatusHudContainer`, `${interactiveMode}#AnchoredLiveContainer`, "component:packages/coding-agent/src/modes/progress-hud.ts#DownloadActivityHud", "tool:todo"];

export const compactBuiltinCases: BuiltinCase[] = [{
  ...chatCase("todo-compact-status", "Parity initialize the compact todo plan with one actionable task.", [
    {
      name: "Short terminal folds the real todo HUD into the compact status line",
      terminalResize: { columns: 100, rows: 14 },
      browserActions: [{ type: "resize", width: 1200, height: 340 }],
      comparisons: [
        markdownContent("PARITY_TODO_COMPACT_READY"),
        literalContent("TODO", `${todoHud}:has-text("TODO")`, "property:TspSpan.t"),
        literalContent("Parity compact task", `${todoHud}:has-text("Parity compact task")`, "property:TspSpan.t"),
        effect("builtin.todos", "in_progress", "/0/tasks/0/status"),
      ],
    },
  ], compactCovers),
  settings: { "todo.enabled": true, "todo.reminders": false, "tools.approvalMode": "yolo" },
}];

export const providerTurns: Record<string, BuiltinProviderTurn[]> = {
  "chat-todo-compact-status": [
    { match: { textIncludes: "Parity initialize the compact todo plan" }, toolCalls: [{ id: "chat-todo-compact-init", name: "todo", arguments: { op: "init", items: ["Parity compact task"] } }] },
    { match: {}, text: "PARITY_TODO_COMPACT_READY" },
  ],
  "chat-todo-lifecycle": [
    { match: { textIncludes: "Parity initialize the scratch todo plan" }, toolCalls: [{ id: "chat-todo-init", name: "todo", arguments: { op: "init", items: ["Parity actionable task"] } }] },
    { match: {}, text: "PARITY_TODO_STARTED", holdAfterText: true },
    { match: { textIncludes: "Parity complete the scratch todo task." }, toolCalls: [{ id: "chat-todo-done", name: "todo", arguments: { op: "done", task: "Parity actionable task" } }] },
    { match: {}, text: "PARITY_TODO_COMPLETED" },
  ],
};
