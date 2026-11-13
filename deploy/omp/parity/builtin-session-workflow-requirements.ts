import type { BuiltinProviderTurn, BuiltinRequirements } from "./builtin-requirements";

// Loaded by OMP's actual project extension discovery, not the parity fixture's UI factory.
export const sessionWorkflowExtension = `import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";

export default function parityLifecycle(api: ExtensionAPI): void {
  const file = join(process.cwd(), "extension-lifecycle.json");
  const schema = api.zod.object({ loads: api.zod.number().int().nonnegative(), actions: api.zod.array(api.zod.string()) });
  const previous = existsSync(file) ? schema.parse(JSON.parse(readFileSync(file, "utf8"))) : { loads: 0, actions: [] };
  writeFileSync(file, JSON.stringify({ ...previous, loads: previous.loads + 1 }) + "\\n");
  api.registerCommand("parity-extension-action", {
    description: "Persist an invocation through the real extension command registry",
    handler: async (argument, context) => {
      const state = schema.parse(JSON.parse(readFileSync(file, "utf8")));
      state.actions.push(argument);
      writeFileSync(file, JSON.stringify(state) + "\\n");
      context.ui.notify("PARITY_EXTENSION_" + argument.toUpperCase(), "info");
    },
  });
}
`;

const sessionCaptureSources: Readonly<Record<string, string>> = {
  "/savedSessionIndex": "Compare the actual current sessionManager.getSessionFile() resolved path with each manifest.additionalSessionPaths resolved path; return its real zero-based index, -1 when absent. Persist the original manifest through process restart; do not retarget it to the resumed session.",
  "/treeUserTexts": "After manager.flush(), read actual manager.getEntries() in append order, filter message entries with role user, join only text blocks with newline (string content unchanged). Include abandoned sibling branches; do not use live agent context for the full tree.",
  "/treeAssistantTexts": "After manager.flush(), read actual manager.getEntries() in append order, filter assistant message entries, join text blocks with newline. Include abandoned sibling branches, not just manager.getBranch().",
  "/diskUserTexts": "After actual manager.flush(), load CURRENT owned session file from disk through production session parser and persisted-leaf/session-context fold; return active-branch user text strings. Do not infer disk durability from live agent state; honor persisted leaf pointer, not last append order.",
  "/diskAssistantTexts": "After actual manager.flush(), load CURRENT owned session file from disk through production parser and persisted-leaf/session-context fold; return active-branch assistant text strings, joining only text blocks with newline. Do not substitute current in-memory messages.",
  "/originalDiskUserTexts": "Read ORIGINAL manifest.sessionPath through production session parser and persisted-leaf/session-context fold under owned roots; return active-branch user text strings, independent of current fork/resumed identity. Do not change original manifest during restart.",
  "/originalDiskAssistantTexts": "Read ORIGINAL manifest.sessionPath through production session parser and persisted-leaf/session-context fold under owned roots; return active-branch assistant text strings, independent of current fork/resumed identity.",
  "/parentSessionIsOriginal": "Read actual manager.getHeader().parentSession. Compare its id to original manifest.sessionId or, for source paths storing a parent path, its resolved owned path to manifest.sessionPath. Never derive parentage merely from different current session id.",
  "/pluginCommandRegistered": "Read actual AgentSession.slashCommands.some(command => command.name === 'parity-plugin:parity-plugin') after real refreshSlashCommandState has settled. This is the registry consumed by the real input controller, not a fresh discovery call that could mask a stale live registry.",
};

function transcript(requirement: BuiltinRequirements, title: string, userText: string, assistantText: string): void {
  Object.assign(requirement.session, { storage: "persisted", seed: "transcript", title, userText, assistantText });
}

function writeTurns(prompt: string, file: string, marker: string, callId: string): BuiltinProviderTurn[] {
  return [
    { match: { textIncludes: prompt, purpose: "foreground" }, toolCalls: [{ id: callId, name: "write", arguments: { path: file, content: `${marker}\n` } }] },
    { match: { textIncludes: prompt, purpose: "foreground" }, text: marker },
  ];
}

export function applySessionWorkflowRequirements(requirements: Record<string, BuiltinRequirements>): void {
  const ids = [
    "session-resume-selected-restart", "session-branch-sibling-tree-restart", "session-tree-summary-cancel",
    "session-fork-isolated-history-restart", "session-fork-memory-refused", "session-plugin-command-reload-lifecycle",
    "session-extension-registered-restart",
  ];
  for (const id of ids) {
    const requirement = requirements[`builtin-${id}`];
    if (!requirement) throw new Error(`Missing session workflow builtin-${id}`);
    for (const capture of requirement.captures) {
      if (capture.channel !== "builtin.session" || !capture.pointer) continue;
      const source = sessionCaptureSources[capture.pointer];
      if (source) capture.source = source;
    }
    requirement.lifecycle.resumeSameSession = true;
    if (id.includes("restart") || id.includes("plugin-command")) requirement.lifecycle.superviseRealRestart = true;
    switch (id) {
      case "session-resume-selected-restart":
        transcript(requirement, "Parity origin workflow", "PARITY_ORIGIN_QUESTION", "PARITY_ORIGIN_ANSWER");
        requirement.session.additionalSavedSessions = [
          { name: "Parity target workflow", userText: "PARITY_TARGET_QUESTION", assistantText: "PARITY_TARGET_ANSWER" },
          { name: "Parity decoy workflow", userText: "PARITY_DECOY_QUESTION", assistantText: "PARITY_DECOY_ANSWER" },
        ];
        requirement.models.turns = [{ match: { textIncludes: "Parity continue resumed workflow.", purpose: "foreground" }, text: "PARITY_RESUME_CONTINUED" }];
        break;
      case "session-branch-sibling-tree-restart":
        transcript(requirement, "Parity tree workflow", "PARITY_TREE_ROOT_QUESTION", "PARITY_TREE_ROOT_ANSWER");
        requirement.models.turns = [
          { match: { textIncludes: "PARITY_TREE_A_QUESTION", purpose: "foreground" }, text: "PARITY_TREE_A_ANSWER" },
          { match: { textIncludes: "PARITY_TREE_B_QUESTION", purpose: "foreground" }, text: "PARITY_TREE_B_ANSWER" },
        ];
        break;
      case "session-tree-summary-cancel":
        transcript(requirement, "Parity cancel workflow", "PARITY_TREE_ROOT_QUESTION", "PARITY_TREE_ROOT_ANSWER");
        requirement.models.turns = [{ match: { textIncludes: "PARITY_TREE_CANCEL_QUESTION", purpose: "foreground" }, text: "PARITY_TREE_CANCEL_ANSWER" }];
        break;
      case "session-fork-isolated-history-restart":
        transcript(requirement, "Parity fork origin", "PARITY_FORK_ORIGIN_QUESTION", "PARITY_FORK_ORIGIN_ANSWER");
        requirement.localFiles = { "fork-note.txt": "PARITY_FORK_ARTIFACT\n" };
        requirement.models.turns = writeTurns("Parity continue only the fork.", "fork-only.txt", "PARITY_FORK_CHILD_ANSWER", "parity-fork-write");
        // The file content deliberately differs from the final assistant message.
        requirement.models.turns[0].toolCalls = [{ id: "parity-fork-write", name: "write", arguments: { path: "fork-only.txt", content: "PARITY_FORK_CHILD_FILE\n" } }];
        requirement.prerequisites.push("Scrub multiplexer identification environment in both actual hosts; classifyTerminalMultiplexer must naturally return null. Do not stub the classifier.");
        break;
      case "session-fork-memory-refused":
        requirement.session.storage = "memory";
        requirement.session.seed = "empty";
        requirement.session.title = "Parity memory workflow";
        requirement.prerequisites.push("Scrub multiplexer identification environment in both actual hosts; classifyTerminalMultiplexer must naturally return null. Do not stub the classifier.");
        break;
      case "session-plugin-command-reload-lifecycle":
        requirement.prerequisites.push("Create real local git repository ./parity-marketplace with .claude-plugin/marketplace.json name parity-market and plugin parity-plugin version 1.0.0, source ./parity-plugin; plugin has valid .claude-plugin/plugin.json and no executable hooks. Install/reload/uninstall use actual managers.");
        requirement.models.turns = [
          ...writeTurns("Parity plugin content.", "plugin-command.txt", "PARITY_PLUGIN_FIRST", "parity-plugin-first-write"),
          ...writeTurns("Parity plugin content.", "plugin-command.txt", "PARITY_PLUGIN_ENABLED", "parity-plugin-enabled-write"),
          ...writeTurns("Parity plugin content.", "plugin-command.txt", "PARITY_PLUGIN_RESTARTED", "parity-plugin-restarted-write"),
        ];
        break;
      case "session-extension-registered-restart":
        transcript(requirement, "Parity extension workflow", "PARITY_EXTENSION_SEED_QUESTION", "PARITY_EXTENSION_SEED_ANSWER");
        break;
    }
  }
}
