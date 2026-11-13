import { realpath } from "node:fs/promises";
import { isAbsolute, join, relative, resolve, sep } from "node:path";
import type { BuiltinRequirements } from "./builtin-requirements";

export interface ProfileManifest {
  version: 1;
  repoRoot: string;
  sourceRoot: string;
  workspace: string;
  home: string;
  scenario: { id: string; kind: "fixture" | "builtin"; settings: Record<string, unknown>; files: Record<string, string>; requirements?: BuiltinRequirements };
  environment: Record<string, string>;
  launch: { sessionPath?: string; memorySession: boolean; tools?: string[] };
}

export function profileManifestPath(workspace: string): string { return join(workspace, ".parity-profile.json"); }
export function assertOwned(root: string, target: string): void {
  const local = relative(resolve(root), resolve(target));
  if (local === ".." || local.startsWith(`..${sep}`) || isAbsolute(local)) throw new Error(`Parity path escapes owned root ${root}: ${target}`);
}
export async function readProfile(workspace: string): Promise<ProfileManifest> {
  const actualWorkspace = await realpath(workspace);
  const manifestPath = await realpath(profileManifestPath(actualWorkspace));
  assertOwned(actualWorkspace, manifestPath);
  const manifest = await Bun.file(manifestPath).json() as ProfileManifest;
  if (manifest.version !== 1 || await realpath(manifest.workspace) !== actualWorkspace || resolve(manifest.home) !== join(actualWorkspace, ".parity-home")) throw new Error("Invalid owned parity profile manifest");
  if (!manifest.scenario || !["fixture", "builtin"].includes(manifest.scenario.kind) || !/^[a-zA-Z0-9_.-]+$/.test(manifest.scenario.id)) throw new Error("Invalid parity scenario manifest");
  assertOwned(actualWorkspace, await realpath(manifest.home));
  return manifest;
}
