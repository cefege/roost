import type { CaptureState } from "./builtin-capture-state";
import { ownedPath } from "./builtin-capture-state";
import type { BuiltinSeedManifest } from "./builtin-seed";
import { probe } from "./probe";

export async function captureCollab(state: CaptureState, manifest: BuiltinSeedManifest): Promise<unknown> {
  const context = probe.requireInteractiveContext();
  const registry = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/collab/registry")>("packages/coding-agent/src/collab/registry.ts");
  await ownedPath(state, registry.collabHostsRuntimeDir());
  const hosts = await registry.listCollabHosts();
  const controller = context.collabController;
  const host = controller.host;
  const guest = context.collabGuest;
  return {
    hostActive: host !== undefined,
    hostAccess: host?.access ?? null,
    hostGeneration: controller.generation,
    relayConnected: host?.relayConnected ?? false,
    participants: host?.participants.length ?? 0,
    registryHosts: hosts.length,
    registryMatchesCurrent: host !== undefined && hosts.some(entry => entry.sessionId === context.sessionManager.getSessionId()),
    guestActive: guest !== undefined,
    guestReadOnly: guest?.readOnly ?? false,
    guestWelcomed: guest !== undefined && guest.state !== null,
    localSessionRestored: !guest && manifest.sessionPath !== undefined && context.sessionManager.getSessionFile() === manifest.sessionPath,
  };
}
