import { realpath } from "node:fs/promises";
import { resolve } from "node:path";
import type { AgentMessage } from "@omp-source/packages/agent/src/types";
import type { AgentSession } from "@omp-source/packages/coding-agent/src/session/agent-session";
import type { SessionEntry } from "@omp-source/packages/coding-agent/src/session/session-entries";
import type { BuiltinSeedManifest } from "./builtin-seed";
import type { CaptureState } from "./builtin-capture-state";
import { objectValue, ownedPath, requestedPointers } from "./builtin-capture-state";
import { captureGit } from "./builtin-capture-boundaries";

export function messageTexts(messages: readonly AgentMessage[], role: "user" | "assistant"): string[] {
  return messages.flatMap(message => {
    if (message.role !== role) return [];
    return [typeof message.content === "string" ? message.content : message.content.flatMap(block => block.type === "text" ? [block.text] : []).join("\n")];
  });
}

export async function readPersistedEntries(state: CaptureState, file: string): Promise<SessionEntry[]> {
  const loader = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/session/session-loader")>("packages/coding-agent/src/session/session-loader.ts");
  const loaded = await loader.loadSessionFile(await ownedPath(state, file), undefined, { throwIfMissing: true });
  if (loaded.invalidHeader || loaded.malformedRecords > 0) throw new Error(`Invalid persisted session capture: ${file}`);
  return loaded.entries.filter(entry => entry.type !== "session");
}

export async function captureSessionWorkflows(state: CaptureState, session: AgentSession, manifest: BuiltinSeedManifest): Promise<Record<string, unknown>> {
  const pointers = requestedPointers(state, "builtin.session");
  const requested = (name: string): boolean => pointers.some(parts => parts[0] === name);
  const observation: Record<string, unknown> = {};
  const manager = session.sessionManager;
  const file = manager.getSessionFile();
  if (requested("cwdMatchesGitRoot")) {
    const git = objectValue(await captureGit(state), "Actual Git capture");
    observation.cwdMatchesGitRoot = await realpath(await ownedPath(state, manager.getCwd())) === git.root;
  }
  if (requested("savedSessionIndex")) observation.savedSessionIndex = file ? manifest.additionalSessionPaths.findIndex(saved => resolve(saved) === resolve(file)) : -1;
  if (requested("treeUserTexts") || requested("treeAssistantTexts")) {
    const messages = manager.getEntries().flatMap(entry => entry.type === "message" ? [entry.message] : []);
    observation.treeUserTexts = messageTexts(messages, "user");
    observation.treeAssistantTexts = messageTexts(messages, "assistant");
  }
  if (requested("parentSession") || requested("parentSessionIsOriginal")) {
    const parentSession = manager.getHeader()?.parentSession;
    observation.parentSession = parentSession;
    observation.parentSessionIsOriginal = parentSession !== undefined && (parentSession === manifest.sessionId || (manifest.sessionPath !== undefined && resolve(parentSession) === resolve(manifest.sessionPath)));
  }
  if (requested("pluginCommandRegistered") || requested("registeredFileCommands")) {
    observation.pluginCommandRegistered = session.slashCommands.some(command => command.name === "parity-plugin:parity-plugin");
    observation.registeredFileCommands = Object.fromEntries(session.slashCommands.map(command => [command.name, true]));
  }
  for (const [prefix, path] of [["disk", file], ["originalDisk", manifest.sessionPath]] as const) {
    if (!requested(`${prefix}UserTexts`) && !requested(`${prefix}AssistantTexts`)) continue;
    if (!path) throw new Error(`Missing actual session file for ${prefix} transcript capture`);
    const entries = await readPersistedEntries(state, path);
    const contexts = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/session/session-context")>("packages/coding-agent/src/session/session-context.ts");
    const messages = contexts.buildSessionContext(entries).messages;
    observation[`${prefix}UserTexts`] = messageTexts(messages, "user");
    observation[`${prefix}AssistantTexts`] = messageTexts(messages, "assistant");
  }
  return observation;
}
