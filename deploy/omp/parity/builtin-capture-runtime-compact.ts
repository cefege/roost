import type { AgentSession } from "@omp-source/packages/coding-agent/src/session/agent-session";
import type { CaptureState } from "./builtin-capture-state";
import { requestedPointers } from "./builtin-capture-state";
import { readPersistedEntries, messageTexts } from "./builtin-capture-session-workflows";
import { imagePayloads, pngPayload, providerMessages, runtimeProviderRequests, wireText } from "./builtin-capture-runtime-boundaries";

export async function captureRuntimeCompact(state: CaptureState, session: AgentSession): Promise<unknown> {
  const manager = session.sessionManager;
  await manager.flush();
  const file = manager.getSessionFile();
  if (!file) throw new Error("Runtime compact observation requires the actual saved session");
  const entries = await readPersistedEntries(state, file);
  const compactions = entries.filter(entry => entry.type === "compaction");
  const latest = compactions.at(-1);
  const originalUser = state.requirement.session.userText;
  const originalAssistant = state.requirement.session.assistantText;
  if (typeof originalUser !== "string" || typeof originalAssistant !== "string") throw new Error("Runtime compact observation lacks immutable original seed identity");
  const before = latest ? entries.slice(0, entries.indexOf(latest)) : entries;
  const originalMessages = before.flatMap(entry => entry.type === "message" ? [entry.message] : []);
  const observation: Record<string, unknown> = {
    count: compactions.length,
    activeHasOriginalAssistant: messageTexts(session.messages, "assistant").includes(originalAssistant),
    rawHistoryRetained: messageTexts(originalMessages, "user").includes(originalUser) && messageTexts(originalMessages, "assistant").includes(originalAssistant) && latest !== undefined && session.messages.some(message => message.role === "compactionSummary" && message.summary === latest.summary),
  };
  if (latest) { observation.method = latest.method; observation.summary = latest.summary; }
  const fields = requestedPointers(state, "builtin.session").filter(parts => parts[0] === "runtime" && parts[1] === "compact").map(parts => parts[2]);
  if (fields.some(field => ["archiveHasFrames", "archiveSourceRetained", "providerImageReplay"].includes(field))) {
    if (!latest) throw new Error("Actual snapcompact observation lacks a persisted compaction");
    const compact = await state.context.load<typeof import("@omp-source/packages/snapcompact/src/snapcompact")>("packages/snapcompact/src/snapcompact.ts");
    const archive = compact.getPreservedArchive(latest.preserveData);
    if (!archive) throw new Error("Actual persisted compaction has no preserved archive");
    const frames = archive.frames.map(frame => {
      const resolved = manager.resolveFrameData(frame.data);
      if (!resolved && frame.data.startsWith("blob:")) throw new Error("Actual archive frame blob cannot be resolved");
      return pngPayload(resolved ? resolved.read() : frame.data);
    });
    observation.archiveHasFrames = frames.length > 0;
    const source = compact.archiveSourceText(archive);
    observation.archiveSourceRetained = source !== undefined && source.includes(originalUser) && source.includes(originalAssistant);
    if (fields.includes("providerImageReplay")) observation.providerImageReplay = (await runtimeProviderRequests(state)).some(request => request.stream && imagePayloads(request.body).some(payload => frames.some(frame => frame.equals(payload))));
  }
  if (fields.some(field => field === "remoteRequestCount" || field === "remoteFocusIncluded")) {
    const configuration = state.requirement.runtimeWorkflow?.remoteCompaction;
    if (!configuration) throw new Error("Actual remote compact observation lacks its owned endpoint configuration");
    const requests = (await runtimeProviderRequests(state)).filter(request => !request.stream && new URL(request.url).pathname === configuration.path);
    for (const request of requests) {
      const messages = providerMessages(request);
      if (!request.modelId || !messages.some(message => message.role === "system" && wireText(message.content).length > 0) || !messages.some(message => message.role === "user" && wireText(message.content).length > 0)) throw new Error("Actual remote compact request lacks its model or system/user prompt");
    }
    observation.remoteRequestCount = requests.length;
    observation.remoteFocusIncluded = requests.some(request => providerMessages(request).some(message => wireText(message.content).includes("Preserve parity remote evidence")));
  }
  return observation;
}
