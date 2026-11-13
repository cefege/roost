import type { AgentSession } from "@omp-source/packages/coding-agent/src/session/agent-session";
import type { CaptureState } from "./builtin-capture-state";
import { objectValue, requestedPointers } from "./builtin-capture-state";
import { captureSettings } from "./builtin-capture-files";
import { captureMnemopi } from "./builtin-capture-mnemopi";
import { captureSharpshooter } from "./builtin-capture-sharpshooter";

export async function captureMemory(state: CaptureState, session: AgentSession, settingsPath: string): Promise<unknown> {
  if (requestedPointers(state, "builtin.memory").some(parts => parts[0] === "mnemopi")) return { mnemopi: await captureMnemopi(state, session) };
  if (requestedPointers(state, "builtin.memory").some(parts => parts[0] === "sharpshooter")) return { sharpshooter: await captureSharpshooter(state, session) };
  const memory = session.getHindsightSessionState();
  if (memory) await memory.mentalModelsLoadPromise;
  const stored = objectValue(await captureSettings(state, session, settingsPath), "Actual persisted memory settings");
  const memorySettings = objectValue(stored.memory, "Actual persisted memory backend settings");
  const hindsight = objectValue(stored.hindsight, "Actual persisted Hindsight settings");
  const snippet = memory?.mentalModelsSnippet;
  return {
    active: memory !== undefined,
    ...(memory ? { bankId: memory.bankId, mentalModelsEnabled: memory.config.mentalModelsEnabled } : {}),
    snippet: snippet ?? null,
    snippetIncludes: {
      original: typeof snippet === "string" && snippet.includes("PARITY_MEMORY_ORIGINAL"),
      refreshed: typeof snippet === "string" && snippet.includes("PARITY_MEMORY_REFRESHED"),
      seeded: typeof snippet === "string" && snippet.includes("PARITY_MEMORY_SEEDED"),
      curated: typeof snippet === "string" && snippet.includes("PARITY_MEMORY_CURATED"),
    },
    persistedSettings: {
      backend: memorySettings.backend,
      bankId: hindsight.bankId,
      scoping: hindsight.scoping,
      mentalModelsEnabled: hindsight.mentalModelsEnabled,
      autoSeed: hindsight.mentalModelAutoSeed,
    },
  };
}
