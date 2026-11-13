import type { AgentSession } from "@omp-source/packages/coding-agent/src/session/agent-session";
import type { CaptureState } from "./builtin-capture-state";
import { exists, ownedPath, readOwnedText } from "./builtin-capture-state";

export async function captureSharpshooter(state: CaptureState, session: AgentSession): Promise<unknown> {
  const paths = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/sharpshooter/paths")>("packages/coding-agent/src/sharpshooter/paths.ts");
  const queue = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/sharpshooter/queue")>("packages/coding-agent/src/sharpshooter/queue.ts");
  const extraction = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/sharpshooter/extract")>("packages/coding-agent/src/sharpshooter/extract.ts");
  const types = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/sharpshooter/types")>("packages/coding-agent/src/sharpshooter/types.ts");
  const agentDir = await ownedPath(state, session.settings.getAgentDir());
  const cwd = await ownedPath(state, session.sessionManager.getCwd());
  await ownedPath(state, paths.sharpshooterBankDir(agentDir, cwd));
  await extraction.flushSharpshooterExtraction(session);
  const groups = await queue.listSharpshooterDeltas(agentDir, cwd);
  for (const group of groups) for (const entry of group.deltas) await ownedPath(state, entry.file);
  const files: Record<string, string> = {};
  for (const name of types.SHARPSHOOTER_MEMORY_FILES) {
    const file = paths.sharpshooterMemoryFilePath(agentDir, cwd, name);
    if (await exists(state, file)) files[name] = await readOwnedText(state, file);
  }
  return {
    queueDepth: await queue.sharpshooterQueueDepth(agentDir, cwd),
    statements: groups.flatMap(group => group.deltas.map(entry => entry.delta.statement)),
    files,
    state: await paths.readSharpshooterState(agentDir, cwd),
    bankExists: await exists(state, paths.sharpshooterBankDir(agentDir, cwd)),
  };
}
