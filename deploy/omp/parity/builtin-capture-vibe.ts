import { resolve } from "node:path";
import type { AgentSession } from "@omp-source/packages/coding-agent/src/session/agent-session";
import type { ToolSession } from "@omp-source/packages/coding-agent/src/tools";
import type { VibeSpawnLifecycleEvent, VibeTombstoneLifecycleEvent } from "@omp-source/packages/coding-agent/src/vibe/lifecycle";
import type { CaptureState } from "./builtin-capture-state";
import { exists } from "./builtin-capture-state";

export async function captureVibe(state: CaptureState, session: AgentSession, toolSession: ToolSession): Promise<unknown> {
  const runtime = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/vibe/runtime")>("packages/coding-agent/src/vibe/runtime.ts");
  const lifecycle = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/vibe/lifecycle")>("packages/coding-agent/src/vibe/lifecycle.ts");
  const registry = runtime.VibeSessionRegistry.global();
  const scope = registry.ownerScope(toolSession);
  const workers = Object.fromEntries(registry.screens(toolSession).map(worker => [worker.id, worker]));
  const spawns = new Map<string, VibeSpawnLifecycleEvent>();
  const tombstones = new Map<string, VibeTombstoneLifecycleEvent>();
  for (const entry of session.sessionManager.getEntries()) {
    if (entry.type !== "custom" || entry.customType !== lifecycle.VIBE_LIFECYCLE_CUSTOM_TYPE) continue;
    const event = lifecycle.parseLifecycleEvent(entry.data);
    if (!event) throw new Error("Malformed actual Vibe lifecycle entry");
    if (event.ownerId !== scope.ownerId || event.parentSessionId !== scope.parentSessionId) continue;
    if (event.action === "spawn") spawns.set(event.id, event);
    else if (event.action === "tombstone") tombstones.set(event.id, event);
    else if (event.action === "tombstone-revoked" && tombstones.get(event.id)?.reason === "mode-exit") tombstones.delete(event.id);
  }
  const childFiles: Record<string, { exists: boolean }> = {};
  for (const spawn of spawns.values()) {
    if (!scope.parentSessionFile) throw new Error("Actual Vibe spawn has no persisted parent session file");
    if (!/^[A-Za-z0-9_-]+$/.test(spawn.id) || spawn.childSessionFile !== `${spawn.id}.jsonl`) throw new Error("Invalid actual Vibe child session path");
    const child = resolve(scope.parentSessionFile.slice(0, -6), spawn.childSessionFile);
    Object.defineProperty(childFiles, spawn.id, { enumerable: true, value: { exists: await exists(state, child) } });
  }
  return { workers, tombstones: Object.fromEntries(tombstones), childFiles };
}
