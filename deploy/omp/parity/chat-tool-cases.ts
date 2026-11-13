import type { BuiltinCase, Comparison } from "./scenario";
import type { BuiltinProviderTurn } from "./builtin-requirements";
import { composer, effect } from "./builtin-steps";
import { chatCase, content, literalContent, markdownContent, settled, submit, toolComponent } from "./chat-steps";

const readBody = ["PARITY_READ_HEAD", ...Array.from({ length: 18 }, (_, index) => `parity-line-${String(index + 2).padStart(2, "0")}`), "PARITY_READ_TAIL", ""].join("\n");
const editedBody = readBody.replace("parity-line-02", "PARITY_EDITED_LINE");
const readTool = '[data-omp-role="omp.tool.read"]';
const toolTitle = "property:TspToolProps.title";
const toolTarget = "property:TspToolProps.target";
const toolCollapsed = "property:TspToolProps.collapsed";

function expansion(value: "true" | "false", checkpoint: string): Comparison {
  return {
    name: `Real tool output expansion ${value}`,
    terminal: { type: "effect", channel: "builtin.session", pointer: "/toolOutputExpandedAttribute" },
    browser: { type: "dom", selector: `${readTool} button[aria-expanded]`, read: "attribute", attribute: "aria-expanded" },
    expected: value,
    proves: [{
      entry: toolCollapsed, effect: "interaction", value: value === "false",
      target: { checkpoint, comparison: "Read" },
      assertionGroups: [{ comparisons: [
        { checkpoint, comparison: `Real tool output expansion ${value}` },
        { checkpoint, comparison: "builtin.session/toolOutputExpandedAttribute" },
      ] }],
    }],
  };
}

export const builtinCases: BuiltinCase[] = [
  {
    ...chatCase("coding-tools", "Parity read parity.txt, run the scratch shell command, then edit its second line.", [
      settled("PARITY_CODING_COMPLETE", [
        markdownContent("PARITY_CODING_COMPLETE"),
        effect("builtin.session", "read", "/messages/2/toolName"), effect("builtin.session", false, "/messages/2/isError"),
        effect("builtin.session", "bash", "/messages/4/toolName"), effect("builtin.session", false, "/messages/4/isError"),
        effect("builtin.session", "edit", "/messages/6/toolName"), effect("builtin.session", false, "/messages/6/isError"),
        effect("builtin.session", 8, "/messageCount"),
        effect("builtin.workspace-file", editedBody, "/parity.txt/content"),
        effect("builtin.workspace-file", "PARITY_BASH_EFFECT\n", "/parity-shell.txt/content"),
        literalContent("Read", `${readTool} strong:text-is("Read")`, toolTitle),
        literalContent("parity.txt", readTool, toolTarget),
      ]),
      {
        name: "Real keyboard expansion exposes the read result tail",
        terminalInput: "\u000f",
        browserActions: [{ type: "press", selector: composer, key: "Control+o" }],
        comparisons: [
          literalContent("Read", `${readTool} button[aria-expanded]`, toolTitle),
          content("PARITY_READ_TAIL", readTool),
          expansion("true", "Real keyboard expansion exposes the read result tail"),
          effect("builtin.session", "true", "/toolOutputExpandedAttribute"),
        ],
      },
      {
        name: "Real keyboard collapse updates the native fold control",
        terminalInput: "\u000f",
        browserActions: [{ type: "press", selector: composer, key: "Control+o" }],
        comparisons: [
          content("PARITY_CODING_COMPLETE"),
          literalContent("Read", `${readTool} button[aria-expanded]`, toolTitle),
          expansion("false", "Real keyboard collapse updates the native fold control"),
          effect("builtin.session", "false", "/toolOutputExpandedAttribute"),
          effect("builtin.workspace-file", editedBody, "/parity.txt/content"),
        ],
      },
    ], [toolComponent, "tool:read", "tool:bash", "tool:edit", toolTitle, toolTarget, toolCollapsed]),
    files: { "parity.txt": readBody },
    settings: { "edit.mode": "replace", "tools.approvalMode": "yolo", "bash.allowCompoundCommands": true },
  },
  chatCase("tool-error", "Parity attempt the missing scratch file and report its tool error.", [
    settled("PARITY_TOOL_ERROR_ACKNOWLEDGED", [
      markdownContent("PARITY_TOOL_ERROR_ACKNOWLEDGED"),
      literalContent("Read", `${readTool} strong:text-is("Read")`, toolTitle),
      literalContent("parity-missing.txt", readTool, toolTarget),
      effect("builtin.session", "read", "/messages/2/toolName"), effect("builtin.session", true, "/messages/2/isError"),
      effect("builtin.workspace-file", false, "/parity-missing.txt/exists"),
    ]),
  ], [toolComponent, "tool:read", toolTitle, toolTarget]),
  {
    ...chatCase("tool-progress-abort", "Parity start the slow scratch shell command, then wait for my interruption.", [
      { name: "Actual process output streams through the running tool component", comparisons: [content("PARITY_BASH_PARTIAL", '[data-omp-role="omp.tool.bash"]'), effect("builtin.session", true, "/isStreaming"), effect("builtin.workspace-file", false, "/parity-should-not-exist.txt/exists")] },
      {
        name: "Actual interruption settles the foreground process without its later effect",
        terminalInput: "\u001b", browserActions: [{ type: "press", selector: composer, key: "Escape" }],
        comparisons: [content("PARITY_BASH_PARTIAL", '[data-omp-role="omp.tool.bash"]'), effect("builtin.session", false, "/isStreaming"), effect("builtin.workspace-file", false, "/parity-should-not-exist.txt/exists")],
      },
      submit("Parity continue after cancelling the scratch process.", "PARITY_AFTER_TOOL_ABORT", [markdownContent("PARITY_AFTER_TOOL_ABORT"), effect("builtin.workspace-file", false, "/parity-should-not-exist.txt/exists")]),
    ], [toolComponent, "tool:bash"]),
    settings: { "tools.approvalMode": "yolo", "bash.allowCompoundCommands": true, "bash.autoBackground.enabled": false },
  },
];

export const providerTurns: Record<string, BuiltinProviderTurn[]> = {
  "chat-coding-tools": [
    { match: { textIncludes: "Parity read parity.txt" }, toolCalls: [{ id: "chat-read", name: "read", arguments: { path: "parity.txt" }, argumentChunks: ['{"path":', '"parity.txt"}'] }] },
    { match: {}, toolCalls: [{ id: "chat-bash", name: "bash", arguments: { command: "printf 'PARITY_BASH_EFFECT\\n' > parity-shell.txt; cat parity-shell.txt", timeout: 10 } }] },
    { match: {}, toolCalls: [{ id: "chat-edit", name: "edit", arguments: { path: "parity.txt", old_string: "parity-line-02", new_string: "PARITY_EDITED_LINE" } }] },
    { match: {}, text: "PARITY_CODING_COMPLETE" },
  ],
  "chat-tool-error": [
    { match: { textIncludes: "Parity attempt the missing scratch file" }, toolCalls: [{ id: "chat-missing-read", name: "read", arguments: { path: "parity-missing.txt" } }] },
    { match: {}, text: "PARITY_TOOL_ERROR_ACKNOWLEDGED" },
  ],
  "chat-tool-progress-abort": [
    { match: { textIncludes: "Parity start the slow scratch shell command" }, toolCalls: [{ id: "chat-slow-bash", name: "bash", arguments: { command: "printf 'PARITY_BASH_PARTIAL\\n'; sleep 30; printf 'must not run\\n' > parity-should-not-exist.txt", timeout: 60 } }] },
    { match: { textIncludes: "Parity continue after cancelling the scratch process." }, text: "PARITY_AFTER_TOOL_ABORT" },
  ],
};
