import { BUILTIN_SLASH_COMMAND_DEFS } from "@omp-source/packages/coding-agent/src/slash-commands/builtin-registry";
import type { BuiltinCase, BrowserAction } from "./scenario";
import { commandCase, composer, effect, literalPattern, observed, send } from "./builtin-steps";

export const builtinCompletionCases: BuiltinCase[] = [];
for (const definition of BUILTIN_SLASH_COMMAND_DEFS) {
  for (const subcommand of definition.subcommands ?? []) {
    const prefix = subcommand.name.includes(" ") ? subcommand.name.split(" ")[0] ?? subcommand.name : subcommand.name;
    const matches = (definition.subcommands ?? []).filter(candidate => candidate.name.startsWith(prefix));
    const selectedIndex = matches.findIndex(candidate => candidate.name === subcommand.name);
    const arrows = Math.max(0, selectedIndex);
    const completed = `/${definition.name} ${subcommand.name} `;
    const actions: BrowserAction[] = [{ type: "fill", selector: composer, value: `/${definition.name} ${prefix}` }];
    for (let index = 0; index < arrows; index++) actions.push({ type: "press", selector: composer, key: "ArrowDown" });
    actions.push({ type: "press", selector: composer, key: "Tab" });
    builtinCompletionCases.push({
      id: `builtin-completion-registry-${definition.name}-${subcommand.name.replaceAll(" ", "-")}`,
      covers: ["command:retry"], command: "/retry",
      checkpoints: [observed("Nothing to retry"), {
        name: `Accept actual ${definition.name}/${subcommand.name} completion`,
        terminalInput: `/${definition.name} ${prefix}${"\u001b[B".repeat(arrows)}\t`, browserActions: actions,
        comparisons: [{ name: "Inserted registered subcommand", terminal: { type: "screen", pattern: `(${literalPattern(completed)})`, group: 1 }, browser: { type: "dom", selector: composer, read: "value" }, expected: completed }],
      }, {
        ...send("/retry", "Nothing to retry"), name: "Dismiss completion without submitting it", terminalInput: "\u001b[27u\u0015/retry\r",
      }],
    });
  }
}

const runtimeCompletions: ReadonlyArray<[string, string, string, string]> = [
  ["switch-provider", "/switch openai/parity-s", "/switch openai/parity-small ", "Model set to openai/parity-small."],
  ["switch-role", "/switch @sm", "/switch @smol ", "Model set to openai/parity-small."],
  ["mcp-server", "/mcp disable par", "/mcp disable parity ", 'Disabled "parity" (project config)'],
  ["mcp-remove-scope", "/mcp remove par", "/mcp remove parity --scope user ", 'Removed server "parity" from user config'],
  ["local-url", "/local local://par", "/local local://parity.txt", "Closed local://parity.txt in editor."],
  ["move-directory", "/move parity-d", "/move parity-destination/", "Moved to"],
];
for (const [id, prefix, completed, text] of runtimeCompletions) {
  const mutation = id.startsWith("mcp-") ? [effect("builtin.mcp", id === "mcp-server" ? false : true, id === "mcp-server" ? "/parity/config/enabled" : "/parity/removed")] : [];
  builtinCompletionCases.push(commandCase(`completion-runtime-${id}`, "/retry", "Nothing to retry", [{
    name: `Complete ${prefix}`, terminalInput: `${prefix}\t`, browserActions: [{ type: "fill", selector: composer, value: prefix }, { type: "press", selector: composer, key: "Tab" }],
    comparisons: [{ name: "Inserted runtime completion", terminal: { type: "screen", pattern: `(${literalPattern(completed)})`, group: 1 }, browser: { type: "dom", selector: composer, read: "value" }, expected: completed }],
  }, { ...observed(text, mutation), terminalInput: "\r", browserActions: [{ type: "press", selector: composer, key: "Enter" }] }]));
}
