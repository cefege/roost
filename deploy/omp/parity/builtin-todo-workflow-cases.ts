import type { BuiltinCase } from "./scenario";
import { commandCase, effect, send } from "./builtin-steps";

const importRoundtrip = commandCase("todo-import-roundtrip", "/todo append parity", "Appended to Todos: Parity", [
  send("/todo export parity-todos.md", "Wrote todos to"),
  send("/todo rm", "Cleared all todos.", [effect("builtin.todos", [])]),
  send("/todo import parity-todos.md", "Imported 1 phase(s), 1 task(s) from"),
]);

export const todoWorkflowCases: BuiltinCase[] = [importRoundtrip];
