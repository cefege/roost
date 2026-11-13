import type { BehaviorClaim, BuiltinCase, Checkpoint, Comparison } from "./scenario";
import { commandCase, composer, effect, observed, send } from "./builtin-steps";
import { sessionWorkflowExtension } from "./builtin-session-workflow-requirements";

function claim(checkpoint: Checkpoint, ...entries: string[]): Checkpoint {
  return {
    ...checkpoint,
    comparisons: checkpoint.comparisons.map(comparison => ({
      ...comparison,
      proves: entries.map((entry): BehaviorClaim => ({ entry, effect: comparison.terminal.type === "effect" || comparison.terminal.type === "file" ? "host" : "interaction", value: comparison.expected })),
    })),
  };
}

function workflow(id: string, command: string, opening: Checkpoint, steps: Checkpoint[]): BuiltinCase {
  const scenario = commandCase(id, command, opening.name, steps);
  scenario.checkpoints[0] = { ...opening, name: `1. ${opening.name}` };
  scenario.covers = [...new Set(scenario.checkpoints.flatMap(checkpoint => checkpoint.comparisons.flatMap(comparison => comparison.proves?.map(proof => proof.entry) ?? [])))];
  return scenario;
}

function session(pointer: string, expected: unknown): Comparison {
  return effect("builtin.session", expected, pointer);
}

function draft(text: string): Comparison {
  return { name: "Actual restored user draft", terminal: { type: "screen", pattern: `(${text})`, group: 1 }, browser: { type: "dom", selector: composer, read: "value" }, expected: text };
}

function replaceDraft(command: string, text: string, comparisons: Comparison[] = []): Checkpoint {
  const checkpoint = send(command, text, comparisons);
  return { ...checkpoint, terminalInput: `\u0015${checkpoint.terminalInput}` };
}

function filterSession(title: string, comparisons: Comparison[] = []): Checkpoint {
  return {
    ...observed(title, comparisons), terminalInput: title,
    browserActions: [{ type: "fill", selector: 'input[placeholder^="Search sessions"], input[placeholder="Search all sessions…"]', value: title }],
  };
}

function selectSession(comparisons: Comparison[]): Checkpoint {
  return {
    ...observed("Resumed session", comparisons), terminalInput: "\r",
    browserActions: [{ type: "click", selector: 'button:text-is("Resume")' }],
  };
}

function filterTree(text: string, comparisons: Comparison[] = []): Checkpoint {
  return {
    ...observed(text, comparisons), terminalInput: text,
    browserActions: [{ type: "fill", selector: 'input[placeholder="Search entries…"]', value: text }],
  };
}

function switchTree(text: string, comparisons: Comparison[]): Checkpoint {
  return {
    ...observed(text, comparisons), terminalInput: "\r",
    browserActions: [{ type: "click", selector: 'button:text-is("Switch")' }],
  };
}

const resume = "command:resume";
const tree = "command:tree";
const branch = "command:branch";
const fork = "command:fork";
const reload = "command:reload-plugins";
const restart = "command:restart";

export const sessionWorkflowCases: BuiltinCase[] = [
  workflow("session-resume-selected-restart", "/resume", claim(observed("Parity target workflow", [session("/sameSessionId", true)]), resume), [
    claim(filterSession("Parity target workflow", [session("/savedSessionIndex", -1)]), resume),
    claim({ ...observed("PARITY_ORIGIN_ANSWER", [session("/sameSessionId", true), session("/diskUserTexts", ["PARITY_ORIGIN_QUESTION"])]), terminalInput: "\u001b", browserActions: [{ type: "press", key: "Escape" }] }, resume),
    claim(send("/resume", "Parity target workflow"), resume),
    claim(filterSession("Parity target workflow"), resume),
    claim(selectSession([session("/savedSessionIndex", 0), session("/sameSessionId", false), session("/userTexts", ["PARITY_TARGET_QUESTION"]), session("/diskAssistantTexts", ["PARITY_TARGET_ANSWER"])]), resume),
    claim(observed("PARITY_TARGET_ANSWER", [session("/persistedTitle", "Parity target workflow")]), resume),
    claim(send("Parity continue resumed workflow.", "PARITY_RESUME_CONTINUED", [
      session("/savedSessionIndex", 0), session("/diskUserTexts", ["PARITY_TARGET_QUESTION", "Parity continue resumed workflow."]),
      session("/diskAssistantTexts", ["PARITY_TARGET_ANSWER", "PARITY_RESUME_CONTINUED"]),
      session("/originalDiskUserTexts", ["PARITY_ORIGIN_QUESTION"]), session("/originalDiskAssistantTexts", ["PARITY_ORIGIN_ANSWER"]),
      effect("builtin.provider", 1, "/requestCount"),
    ]), resume),
    claim(send("/restart", "PARITY_RESUME_CONTINUED", [effect("builtin.process", 1, "/restartCount"), session("/savedSessionIndex", 0), session("/persistedTitle", "Parity target workflow"), session("/diskUserTexts", ["PARITY_TARGET_QUESTION", "Parity continue resumed workflow."])]), resume, restart),
    claim(send("/resume", "Parity origin workflow"), resume),
    claim(filterSession("Parity origin workflow"), resume),
    claim(selectSession([session("/sameSessionId", true), session("/savedSessionIndex", -1), session("/userTexts", ["PARITY_ORIGIN_QUESTION"])]), resume),
    claim(observed("PARITY_ORIGIN_ANSWER", [session("/diskAssistantTexts", ["PARITY_ORIGIN_ANSWER"])]), resume),
  ]),
  {
    ...workflow("session-branch-sibling-tree-restart", "/branch", claim(observed("PARITY_TREE_ROOT_QUESTION"), branch), [
      claim({ ...observed("PARITY_TREE_ROOT_ANSWER", [session("/sameSessionId", true), session("/diskUserTexts", ["PARITY_TREE_ROOT_QUESTION"])]), terminalInput: "\u001b", browserActions: [{ type: "click", selector: '[data-omp-role="omp.rewind.bar"] button:text-is("Cancel")' }] }, branch),
      claim(send("PARITY_TREE_A_QUESTION", "PARITY_TREE_A_ANSWER"), branch),
      claim(send("/branch", "PARITY_TREE_A_QUESTION"), branch),
      claim({ ...observed("PARITY_TREE_A_QUESTION"), terminalInput: "fPARITY_TREE_A_QUESTION", browserActions: [{ type: "click", selector: '[data-omp-role="omp.rewind.bar"] button:text-is("Filter")' }, { type: "fill", selector: '[data-omp-role="omp.rewind.bar"] input', value: "PARITY_TREE_A_QUESTION" }] }, branch),
      claim({ ...observed("Rewound to selected point", [draft("PARITY_TREE_A_QUESTION"), session("/sameSessionId", true), session("/userTexts", ["PARITY_TREE_ROOT_QUESTION"]), session("/treeUserTexts", ["PARITY_TREE_ROOT_QUESTION", "PARITY_TREE_A_QUESTION"])]), terminalInput: "\r", browserActions: [{ type: "click", selector: '[data-omp-role="omp.rewind.bar"] button:text-is("Rewind here")' }] }, branch),
      claim(replaceDraft("PARITY_TREE_B_QUESTION", "PARITY_TREE_B_ANSWER", [
        session("/userTexts", ["PARITY_TREE_ROOT_QUESTION", "PARITY_TREE_B_QUESTION"]),
        session("/treeUserTexts", ["PARITY_TREE_ROOT_QUESTION", "PARITY_TREE_A_QUESTION", "PARITY_TREE_B_QUESTION"]),
        session("/treeAssistantTexts", ["PARITY_TREE_ROOT_ANSWER", "PARITY_TREE_A_ANSWER", "PARITY_TREE_B_ANSWER"]),
        session("/diskUserTexts", ["PARITY_TREE_ROOT_QUESTION", "PARITY_TREE_B_QUESTION"]),
      ]), branch),
      claim(send("/restart", "PARITY_TREE_B_ANSWER", [effect("builtin.process", 1, "/restartCount"), session("/sameSessionId", true), session("/userTexts", ["PARITY_TREE_ROOT_QUESTION", "PARITY_TREE_B_QUESTION"]), session("/treeUserTexts", ["PARITY_TREE_ROOT_QUESTION", "PARITY_TREE_A_QUESTION", "PARITY_TREE_B_QUESTION"])]), branch, restart),
      claim(send("/tree", "PARITY_TREE_A_QUESTION"), tree),
      claim(filterTree("PARITY_TREE_A_ANSWER"), tree),
      claim(switchTree("Navigated to selected point", [session("/sameSessionId", true), session("/userTexts", ["PARITY_TREE_ROOT_QUESTION", "PARITY_TREE_A_QUESTION"]), session("/diskAssistantTexts", ["PARITY_TREE_ROOT_ANSWER", "PARITY_TREE_A_ANSWER"])]), tree),
      claim(observed("PARITY_TREE_A_ANSWER"), tree),
      claim(send("/tree", "PARITY_TREE_B_QUESTION"), tree),
      claim(filterTree("PARITY_TREE_B_ANSWER"), tree),
      claim(switchTree("Navigated to selected point", [session("/userTexts", ["PARITY_TREE_ROOT_QUESTION", "PARITY_TREE_B_QUESTION"]), session("/diskAssistantTexts", ["PARITY_TREE_ROOT_ANSWER", "PARITY_TREE_B_ANSWER"]), session("/treeUserTexts", ["PARITY_TREE_ROOT_QUESTION", "PARITY_TREE_A_QUESTION", "PARITY_TREE_B_QUESTION"])]), tree),
      claim(observed("PARITY_TREE_B_ANSWER", [effect("builtin.provider", 2, "/requestCount")]), tree),
    ]),
    settings: { "branchSummary.enabled": false },
  },
  { ...workflow("session-tree-summary-cancel", "/queue PARITY_TREE_CANCEL_QUESTION", claim(observed("PARITY_TREE_CANCEL_ANSWER"), "command:queue"), [
    claim(send("/tree", "PARITY_TREE_ROOT_QUESTION"), tree),
    claim(filterTree("PARITY_TREE_ROOT_ANSWER"), tree),
    claim(switchTree("Summarize branch?", [session("/userTexts", ["PARITY_TREE_ROOT_QUESTION", "PARITY_TREE_CANCEL_QUESTION"])]), tree),
    claim({ ...observed("PARITY_TREE_ROOT_QUESTION", [session("/sameSessionId", true), session("/diskAssistantTexts", ["PARITY_TREE_ROOT_ANSWER", "PARITY_TREE_CANCEL_ANSWER"])]), terminalInput: "\u001b", browserActions: [{ type: "press", key: "Escape" }] }, tree),
    claim({ ...observed("PARITY_TREE_CANCEL_ANSWER", [session("/userTexts", ["PARITY_TREE_ROOT_QUESTION", "PARITY_TREE_CANCEL_QUESTION"]), effect("builtin.provider", 1, "/requestCount")]), terminalInput: "\u001b", browserActions: [{ type: "click", selector: 'button:text-is("Close")' }] }, tree),
  ]), settings: { "branchSummary.enabled": true } },
  workflow("session-fork-isolated-history-restart", "/fork", claim(observed("Session forked", [
    session("/sameSessionId", false), session("/parentSessionIsOriginal", true), session("/originalFileExists", true),
    session("/diskUserTexts", ["PARITY_FORK_ORIGIN_QUESTION"]), effect("builtin.local-file", "PARITY_FORK_ARTIFACT\n", "/fork-note.txt/content"),
  ]), fork), [
    claim(send("Parity continue only the fork.", "PARITY_FORK_CHILD_ANSWER", [
      session("/diskUserTexts", ["PARITY_FORK_ORIGIN_QUESTION", "Parity continue only the fork."]),
      session("/originalDiskUserTexts", ["PARITY_FORK_ORIGIN_QUESTION"]), session("/originalDiskAssistantTexts", ["PARITY_FORK_ORIGIN_ANSWER"]),
      effect("builtin.workspace-file", "PARITY_FORK_CHILD_FILE\n", "/fork-only.txt/content"), effect("builtin.provider", ["write"], "/toolNames"),
    ]), fork),
    claim(send("/rename Parity fork child", 'Session renamed to "Parity fork child".'), "command:rename"),
    claim(send("/restart", "PARITY_FORK_CHILD_ANSWER", [effect("builtin.process", 1, "/restartCount"), session("/sameSessionId", false), session("/parentSessionIsOriginal", true), session("/persistedTitle", "Parity fork child"), session("/diskUserTexts", ["PARITY_FORK_ORIGIN_QUESTION", "Parity continue only the fork."]), effect("builtin.local-file", "PARITY_FORK_ARTIFACT\n", "/fork-note.txt/content")]), fork, restart),
    claim(send("/resume", "Parity fork origin"), resume),
    claim(filterSession("Parity fork origin"), resume),
    claim(selectSession([session("/sameSessionId", true), session("/userTexts", ["PARITY_FORK_ORIGIN_QUESTION"]), session("/diskAssistantTexts", ["PARITY_FORK_ORIGIN_ANSWER"])]), fork, resume),
    claim(observed("PARITY_FORK_ORIGIN_ANSWER", [effect("builtin.workspace-file", "PARITY_FORK_CHILD_FILE\n", "/fork-only.txt/content"), effect("builtin.local-file", "PARITY_FORK_ARTIFACT\n", "/fork-note.txt/content")]), fork, resume),
  ]),
  workflow("session-fork-memory-refused", "/fork", claim(observed("Fork failed (session not persisted or cancelled)", [session("/messageCount", 0)]), fork), []),
  workflow("session-plugin-command-reload-lifecycle", "/marketplace add ./parity-marketplace", claim(observed("Added marketplace: parity-market", [session("/pluginCommandRegistered", false)]), "command:marketplace", "subcommand:marketplace add"), [
    claim(send("/marketplace install --scope project parity-plugin@parity-market", "Installed parity-plugin from parity-market", [effect("builtin.plugins", true, "/parity-plugin@parity-market/installed")]), "command:marketplace", "subcommand:marketplace install"),
    claim(send("/reload-plugins", "Plugins reloaded.", [session("/pluginCommandRegistered", true)]), reload),
    claim(send("/parity-plugin:parity-plugin", "PARITY_PLUGIN_FIRST", [effect("builtin.workspace-file", "PARITY_PLUGIN_FIRST\n", "/plugin-command.txt/content"), effect("builtin.provider", ["write"], "/toolNames")]), reload),
    claim(send("/plugins disable --scope project parity-plugin@parity-market", "Disabled parity-plugin@parity-market", [session("/pluginCommandRegistered", false), effect("builtin.plugins", false, "/parity-plugin@parity-market/enabled")]), "command:plugins", "subcommand:plugins disable"),
    claim(send("/plugin enable --scope project parity-plugin@parity-market", "Enabled parity-plugin@parity-market", [session("/pluginCommandRegistered", true), effect("builtin.plugins", true, "/parity-plugin@parity-market/enabled")]), "command:plugin", "subcommand:plugin enable"),
    claim(send("/parity-plugin:parity-plugin", "PARITY_PLUGIN_ENABLED", [effect("builtin.workspace-file", "PARITY_PLUGIN_ENABLED\n", "/plugin-command.txt/content")]), "command:plugin", "subcommand:plugin enable"),
    claim(send("/restart", "PARITY_PLUGIN_ENABLED", [effect("builtin.process", 1, "/restartCount"), session("/pluginCommandRegistered", true), effect("builtin.plugins", true, "/parity-plugin@parity-market/enabled")]), restart, reload),
    claim(send("/parity-plugin:parity-plugin", "PARITY_PLUGIN_RESTARTED", [effect("builtin.workspace-file", "PARITY_PLUGIN_RESTARTED\n", "/plugin-command.txt/content"), effect("builtin.provider", 6, "/requestCount")]), restart, reload),
    claim(send("/marketplace uninstall --scope project parity-plugin@parity-market", "Uninstalled parity-plugin@parity-market", [effect("builtin.plugins", false, "/parity-plugin@parity-market/installed")]), "command:marketplace", "subcommand:marketplace uninstall"),
    claim(send("/reload-plugins", "Plugins reloaded.", [session("/pluginCommandRegistered", false), effect("builtin.workspace-file", "PARITY_PLUGIN_RESTARTED\n", "/plugin-command.txt/content")]), reload),
  ]),
  {
    ...workflow("session-extension-registered-restart", "/extensions", claim(observed("Extension Control Center"), "command:extensions"), [
      claim({ ...send("/parity-extension-action before", "PARITY_EXTENSION_BEFORE", [effect("builtin.workspace-file", 1, "/extension-lifecycle.json/json/loads"), effect("builtin.workspace-file", ["before"], "/extension-lifecycle.json/json/actions")]), terminalInput: "\u001b[27u/parity-extension-action before\r", browserActions: [{ type: "press", key: "Escape" }, { type: "fill", selector: composer, value: "/parity-extension-action before" }, { type: "press", selector: composer, key: "Enter" }] }, "command:extensions"),
      claim(send("/reload-plugins", "Plugins reloaded."), reload),
      claim(send("/parity-extension-action after-plugins", "PARITY_EXTENSION_AFTER-PLUGINS", [effect("builtin.workspace-file", 1, "/extension-lifecycle.json/json/loads"), effect("builtin.workspace-file", ["before", "after-plugins"], "/extension-lifecycle.json/json/actions")]), reload),
      claim(send("/restart", "PARITY_EXTENSION_SEED_ANSWER", [effect("builtin.process", 1, "/restartCount"), session("/sameSessionId", true), effect("builtin.workspace-file", 2, "/extension-lifecycle.json/json/loads")]), restart),
      claim(send("/parity-extension-action after-restart", "PARITY_EXTENSION_AFTER-RESTART", [effect("builtin.workspace-file", 2, "/extension-lifecycle.json/json/loads"), effect("builtin.workspace-file", ["before", "after-plugins", "after-restart"], "/extension-lifecycle.json/json/actions"), session("/diskUserTexts", ["PARITY_EXTENSION_SEED_QUESTION"])]), restart),
    ]),
    files: { ".omp/extensions/parity-lifecycle.ts": sessionWorkflowExtension },
  },
];
