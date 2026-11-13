import * as fs from "node:fs/promises";
import * as path from "node:path";
import type { AssistantMessage } from "@omp-source/packages/ai/src/types";
import type { SessionManager } from "@omp-source/packages/coding-agent/src/session/session-manager";
import type { BuiltinRequirements } from "./builtin-requirements";
import { prepareBuiltinServiceFiles, serviceSpecsForRequirements, type BuiltinServiceSpec } from "./fixture-services";
import { seedBuiltinPrerequisites } from "./builtin-seed-stores";

export interface BuiltinSeedContext {
  sourceRoot: string;
  workspace: string;
  home: string;
  sessionPath?: string;
  load<Module>(sourceRelativePath: string): Promise<Module>;
}

export interface BuiltinSeedManifest {
  sessionPath?: string;
  sessionId?: string;
  additionalSessionPaths: string[];
  settingsPath: string;
  environment: Record<string, string>;
  services: BuiltinServiceSpec[];
  unsetEnvironment: string[];
  launch: {
    memorySession: boolean;
    omitTools: string[];
    allowExit: boolean;
    superviseRealRestart: boolean;
    resumeSameSession: boolean;
  };
}

export function builtinSeedManifestPath(home: string): string {
  return path.join(home, ".parity", "builtin-seed.json");
}

export async function readBuiltinSeedManifest(home: string): Promise<BuiltinSeedManifest> {
  return await Bun.file(builtinSeedManifestPath(home)).json() as BuiltinSeedManifest;
}

export function ownedBuiltinPath(root: string, relative: string): string {
  const resolved = path.resolve(root, relative);
  if (resolved !== path.resolve(root) && !resolved.startsWith(`${path.resolve(root)}${path.sep}`)) {
    throw new Error(`Builtin fixture path escapes its owned root: ${relative}`);
  }
  return resolved;
}

export async function writeOwnedBuiltinFile(root: string, relative: string, content: string): Promise<string> {
  const file = ownedBuiltinPath(root, relative);
  await fs.mkdir(path.dirname(file), { recursive: true });
  await fs.writeFile(file, content);
  return file;
}

function assistantMessage(requirement: BuiltinRequirements, text: string): AssistantMessage {
  return {
    role: "assistant", content: [{ type: "text", text }], api: requirement.models.api,
    provider: requirement.models.provider, model: requirement.models.defaultId,
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
    stopReason: "stop", timestamp: 1_780_000_001_000,
  };
}

async function populateSession(
  manager: SessionManager, requirement: BuiltinRequirements,
  transcript: { name: string; userText: string; assistantText: string }, seed: BuiltinRequirements["session"]["seed"],
): Promise<void> {
  manager.appendModelChange(`${requirement.models.provider}/${requirement.models.defaultId}`);
  if (seed !== "empty") {
    const assistant = assistantMessage(requirement, transcript.assistantText);
    if (seed === "heavy-blocks") {
      manager.appendMessage({ role: "user", content: [
        { type: "text", text: transcript.userText },
        { type: "image", mimeType: "image/png", data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=" },
      ], timestamp: 1_780_000_000_000 });
      assistant.content.unshift({ type: "thinking", thinking: "Parity private thought" });
    } else manager.appendMessage({ role: "user", content: transcript.userText, timestamp: 1_780_000_000_000 });
    manager.appendMessage(assistant);
    // Past the 32-message replay chunk, so rebuilds yield to terminal input between chunks.
    if (seed === "heavy-blocks") {
      for (let index = 0; index < 18; index += 1) {
        manager.appendMessage({ role: "user", content: `Parity filler question ${index}`, timestamp: 1_780_000_002_000 + index * 2 });
        manager.appendMessage({ ...assistantMessage(requirement, `Parity filler answer ${index}`), timestamp: 1_780_000_003_000 + index * 2 });
      }
    }
  }
  await manager.setSessionName(transcript.name, "user");
  await manager.ensureOnDisk();
  await manager.flush();
}

export async function seedBuiltin(requirement: BuiltinRequirements, context: BuiltinSeedContext): Promise<void> {
  if (path.resolve(process.env.HOME ?? "") !== path.resolve(context.home)) {
    throw new Error("seedBuiltin must execute in the isolated profile subprocess with HOME set before source imports");
  }
  await fs.mkdir(context.home, { recursive: true });
  await fs.mkdir(context.workspace, { recursive: true });
  process.chdir(context.workspace);
  const { getAgentDir } = await context.load<typeof import("@omp-source/packages/utils/src/dirs")>("packages/utils/src/dirs.ts");
  const agentDir = getAgentDir();
  ownedBuiltinPath(context.home, path.relative(context.home, agentDir));
  await fs.mkdir(agentDir, { recursive: true });
  const { Settings } = await context.load<typeof import("@omp-source/packages/coding-agent/src/config/settings")>("packages/coding-agent/src/config/settings.ts");
  const settings = await Settings.loadIsolated({ cwd: context.workspace, agentDir });
  for (const [role, model] of Object.entries(requirement.models.roles)) settings.setModelRole(role, model);
  await settings.flush();
  const environment: Record<string, string> = { PI_CODING_AGENT_DIR: agentDir, OMP_WORKTREE_DIR: path.join(context.home, ".parity", "worktrees") };
  const manifest: BuiltinSeedManifest = {
    additionalSessionPaths: [], settingsPath: path.join(agentDir, "config.yml"), environment, unsetEnvironment: [],
    services: serviceSpecsForRequirements(requirement),
    launch: { memorySession: requirement.session.storage === "memory", omitTools: [], ...requirement.lifecycle },
  };
  Object.assign(manifest.environment, await prepareBuiltinServiceFiles(manifest.services, context));
  const { SessionManager } = await context.load<typeof import("@omp-source/packages/coding-agent/src/session/session-manager")>("packages/coding-agent/src/session/session-manager.ts");
  if (requirement.session.storage === "memory") {
    if (requirement.session.seed !== "empty" || requirement.localFiles) throw new Error("In-memory seed requires an actual SDK admission path for nonempty history/artifacts");
  } else {
    const manager = SessionManager.create(context.workspace);
    try {
      if (context.sessionPath) {
        const relativeHome = path.relative(context.home, context.sessionPath);
        const relativeWorkspace = path.relative(context.workspace, context.sessionPath);
        if (relativeHome.startsWith("..") && relativeWorkspace.startsWith("..")) throw new Error("Session path is outside owned profile/workspace");
        await manager.setSessionFile(context.sessionPath);
      }
      await populateSession(manager, requirement, { name: requirement.session.title, userText: requirement.session.userText, assistantText: requirement.session.assistantText }, requirement.session.seed);
      manifest.sessionPath = manager.getSessionFile();
      manifest.sessionId = manager.getSessionId();
      if (!manifest.sessionPath) throw new Error("Persisted SessionManager did not produce a session file");
      if (requirement.localFiles) {
        const artifacts = manager.getArtifactsDir();
        if (!artifacts) throw new Error("Persisted session has no artifact directory");
        for (const [name, content] of Object.entries(requirement.localFiles)) await writeOwnedBuiltinFile(artifacts, name, content);
      }
    } finally { await manager.close(); }
  }
  for (const transcript of requirement.session.additionalSavedSessions) {
    const manager = SessionManager.create(context.workspace);
    try {
      await populateSession(manager, requirement, transcript, "transcript");
      const sessionFile = manager.getSessionFile();
      if (!sessionFile) throw new Error("Additional persisted session has no file");
      manifest.additionalSessionPaths.push(sessionFile);
    } finally { await manager.close(); }
  }
  await seedBuiltinPrerequisites(requirement, context, manifest);
  await writeOwnedBuiltinFile(context.home, ".parity/builtin-seed.json", `${JSON.stringify(manifest, null, 2)}\n`);
}
