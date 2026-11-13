import type { Dirent } from "node:fs";
import { readdir } from "node:fs/promises";
import { join } from "node:path";
import { Database } from "bun:sqlite";
import type { AgentSession } from "@omp-source/packages/coding-agent/src/session/agent-session";
import type { CaptureState } from "./builtin-capture-state";
import { assignPath, exists, isMissing, objectValue, ownedPath, readOwnedText, requestedPointers } from "./builtin-capture-state";

export async function captureFiles(state: CaptureState, session: AgentSession, local: boolean): Promise<unknown> {
  const channel = local ? "builtin.local-file" : "builtin.workspace-file";
  const root = local ? session.sessionManager.getArtifactsDir() : session.sessionManager.getCwd();
  if (!root) throw new Error(`${channel}: real artifacts directory unavailable`);
  const result: Record<string, unknown> = {};
  const paths = new Map<string, string[]>();
  for (const pointer of requestedPointers(state, channel)) {
    const field = pointer.findIndex(part => ["content", "json", "exists", "containsSeedTranscript"].includes(part));
    if (field < 1) throw new Error(`Unsupported ${channel} file pointer: /${pointer.join("/")}`);
    const parts = pointer.slice(0, field);
    paths.set(parts.join("/"), parts);
  }
  for (const [file, parts] of paths) {
    const absolute = join(root, file);
    const present = await exists(state, absolute);
    const observation: Record<string, unknown> = { exists: present };
    if (present) {
      const content = await readOwnedText(state, absolute);
      observation.content = content;
      observation.containsSeedTranscript = content.includes(state.requirement.session.userText) && content.includes(state.requirement.session.assistantText);
      if (requestedPointers(state, channel).some(pointer => pointer.slice(0, parts.length).join("/") === file && pointer[parts.length] === "json")) {
        observation.json = JSON.parse(content) as unknown;
      }
    }
    assignPath(result, parts, observation);
  }
  return result;
}

export async function captureSettings(state: CaptureState, session: AgentSession, settingsPath: string): Promise<unknown> {
  await session.settings.flush();
  const bytes = await readOwnedText(state, settingsPath);
  const stored = objectValue(Bun.YAML.parse(bytes), "Persisted owned settings");
  const registry = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/config/registry")>("packages/coding-agent/src/config/registry.ts");
  const result = structuredClone(stored);
  for (const [key, value] of Object.entries(stored)) {
    const setting = registry.lookup(key);
    if (setting) assignPath(result, setting.segments, value);
  }
  for (const parts of requestedPointers(state, "builtin.persisted-settings")) {
    if (parts[parts.length - 1] !== "exists") continue;
    let parent: unknown = result;
    const objectParts = parts.slice(0, -2);
    for (const part of objectParts) {
      if (typeof parent !== "object" || parent === null) { parent = undefined; break; }
      parent = (parent as Record<string, unknown>)[part];
    }
    const key = parts[parts.length - 2];
    const present = typeof parent === "object" && parent !== null && Object.hasOwn(parent, key);
    assignPath(result, parts, present);
  }
  return result;
}

export async function captureApprovals(state: CaptureState, session: AgentSession): Promise<unknown> {
  const module = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/tools/approval-cache")>("packages/coding-agent/src/tools/approval-cache.ts");
  const cache = new module.ApprovalCache(session.settings.getAgentDir());
  if (!await exists(state, cache.filePath)) return [];
  const database = new Database(await ownedPath(state, cache.filePath), { readonly: true });
  try { return database.query<{ key: string; tool: string; approved_at: string }, []>("SELECT key, tool, approved_at FROM grants ORDER BY key").all(); }
  finally { database.close(); }
}

export async function captureRules(state: CaptureState): Promise<unknown> {
  const root = await ownedPath(state, join(state.context.workspace, ".omp", "rules"));
  const rules: Array<{ path: string; content: string }> = [];
  async function visit(directory: string): Promise<void> {
    let entries: Dirent[];
    try { entries = await readdir(await ownedPath(state, directory), { withFileTypes: true }); }
    catch (error) { if (isMissing(error)) return; throw error; }
    for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
      const file = join(directory, entry.name);
      if (entry.isDirectory()) await visit(file);
      else if (entry.isFile() && entry.name.endsWith(".md")) rules.push({ path: file, content: await readOwnedText(state, file) });
    }
  }
  await visit(root);
  return rules;
}
