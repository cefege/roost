import type { AgentSession } from "@omp-source/packages/coding-agent/src/session/agent-session";
import type { CaptureState } from "./builtin-capture-state";
import { ownedPath, readOwnedText, requestedPointers } from "./builtin-capture-state";
import { readPersistedEntries } from "./builtin-capture-session-workflows";
import { providerMessages, runtimeProviderRequests, wireText } from "./builtin-capture-runtime-boundaries";

export async function captureRuntimeShake(state: CaptureState, session: AgentSession): Promise<unknown> {
  await session.sessionManager.flush();
  const file = session.sessionManager.getSessionFile();
  if (!file) throw new Error("Actual shake observation requires its persisted session");
  const entries = await readPersistedEntries(state, file);
  const disk = entries.flatMap(entry => entry.type === "message" && entry.message.role === "toolResult" && entry.message.toolCallId === "parity-shake-read" ? [entry.message] : []);
  const live = session.messages.find(message => message.role === "toolResult" && message.toolCallId === "parity-shake-read");
  if (!live || live.role !== "toolResult" || disk.length === 0) throw new Error("Actual shake read tool result is missing from live or persisted state");
  const latest = disk.at(-1);
  if (!latest) throw new Error("Actual saved shake read tool result is absent");
  const liveText = wireText(live.content);
  const diskText = wireText(latest.content);
  const completeOriginal = (text: string) => text.includes("PARITY_SHAKE_ORIGINAL_START") && text.includes("PARITY_SHAKE_ORIGINAL_END");
  const observation: Record<string, unknown> = {
    prunedReadResults: disk.filter(message => typeof message.prunedAt === "number").length,
    originalInLiveToolResult: completeOriginal(liveText), originalInDiskToolResult: completeOriginal(diskText),
  };
  const fields = requestedPointers(state, "builtin.session").filter(parts => parts[0] === "runtime" && parts[1] === "shake").map(parts => parts[2]);
  if (fields.some(field => field === "recoveryArtifactContainsOriginal" || field === "recoveryPointerOnWire")) {
    const pointer = /\[shaken ~\d+ tokens — recover: artifact:\/\/([0-9]+) \(region ([0-9]+)\)\]/.exec(diskText);
    if (!pointer) throw new Error("Actual elided read result lacks its artifact-backed recovery pointer");
    const path = await session.sessionManager.getArtifactManager()?.getPath(pointer[1]);
    if (!path) throw new Error("Actual shake recovery artifact is missing");
    const artifact = await readOwnedText(state, await ownedPath(state, path));
    const original = await readOwnedText(state, `${state.context.workspace}/shake-source.txt`);
    const section = new RegExp(`^### region ${pointer[2]} \\([^\\n]+\\)\\n\\n([\\s\\S]*?)(?=^### region \\d+ |$(?![\\s\\S]))`, "m").exec(artifact);
    if (!section) throw new Error("Actual shake artifact lacks the referenced recovery section");
    observation.recoveryArtifactContainsOriginal = section[1].includes(original);
    if (fields.includes("recoveryPointerOnWire")) {
      const messages = (await runtimeProviderRequests(state)).flatMap(providerMessages).filter(message => message.role === "tool" && message.tool_call_id === "parity-shake-read");
      const message = messages.at(-1);
      if (!message) throw new Error("Actual provider trace lacks the shaken read result");
      const text = wireText(message.content);
      observation.recoveryPointerOnWire = text.includes(pointer[0]) && !completeOriginal(text);
    }
  }
  return observation;
}
