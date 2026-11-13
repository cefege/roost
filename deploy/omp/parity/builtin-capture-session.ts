import { readdir } from "node:fs/promises";
import { join } from "node:path";
import type { AgentSession } from "@omp-source/packages/coding-agent/src/session/agent-session";
import type { BuiltinSeedManifest } from "./builtin-seed";
import type { CaptureState } from "./builtin-capture-state";
import { exists, isMissing, ownedPath, readOwnedText, requestedPointers } from "./builtin-capture-state";
import { probe } from "./probe";
import { captureSessionWorkflows } from "./builtin-capture-session-workflows";
import { capturePlanWorkflows } from "./builtin-capture-plan-workflows";
import { captureVibe } from "./builtin-capture-vibe";

export async function captureSession(state: CaptureState, session: AgentSession, manifest: BuiltinSeedManifest): Promise<unknown> {
  const manager = session.sessionManager;
  await manager.flush();
  const entries = manager.getBranch();
  const latestCompaction = entries.findLast(entry => entry.type === "compaction");
  const modeState = state.context.extensionContext.sessionModes?.getState();
  const plan = session.getPlanModeState();
  const vibe = session.getVibeModeState();
  const goal = session.getGoalModeState();
  const blocks = session.messages.flatMap<unknown>(message => "content" in message && Array.isArray(message.content) ? message.content : []);
  const observation: Record<string, unknown> = {
    model: session.model,
    thinkingLevel: session.configuredThinkingLevel(),
    messageCount: session.agent.state.messages.length,
    messages: session.agent.state.messages,
    userMessageCount: session.agent.state.messages.filter(message => message.role === "user").length,
    userTexts: session.agent.state.messages.filter(message => message.role === "user").map(message => typeof message.content === "string" ? message.content : message.content.flatMap(block => block.type === "text" ? [block.text] : []).join("\n")),
    assistantMessageCount: session.agent.state.messages.filter(message => message.role === "assistant").length,
    isStreaming: session.isStreaming,
    steeringQueue: session.getQueuedMessages().steering,
    sessionId: manager.getSessionId(),
    persistedTitle: manager.getSessionName(),
    imageBlocks: blocks.filter(block => block && typeof block === "object" && "type" in block && block.type === "image").length,
    thinkingBlocks: blocks.filter(block => block && typeof block === "object" && "type" in block && block.type === "thinking").length,
    prewalkArmed: session.getPrewalkState() !== undefined,
    latestCompaction,
    isCompacting: session.isCompacting,
    followUpQueue: session.getQueuedMessages().followUp,
    additionalDirectoryCount: manager.getAdditionalDirectories().length,
    additionalDirectories: manager.getAdditionalDirectories(),
    mode: plan?.enabled ? "plan" : vibe?.enabled ? "vibe" : goal?.enabled ? "goal" : "none",
    planState: modeState?.plan.status ?? (plan?.enabled ? "active" : "none"),
    handoffCommitted: entries.some(entry => entry.type === "compaction" && entry.method === "handoff"),
  };
  if (manifest.sessionId !== undefined) observation.sameSessionId = manager.getSessionId() === manifest.sessionId;
  if (manifest.sessionPath !== undefined) observation.originalFileExists = await exists(state, manifest.sessionPath);
  const pointers = requestedPointers(state, "builtin.session");
  if (pointers.some(parts => ["paused", "loopEnabled", "todoExpanded"].includes(parts[0]))) Object.assign(observation, probe.interactiveState());
  if (pointers.some(parts => parts[0] === "toolOutputExpandedAttribute")) observation.toolOutputExpandedAttribute = String(probe.requireInteractiveContext().toolOutputExpanded);
  if (pointers.some(parts => parts[0] === "draftImageBlocks")) observation.draftImageBlocks = probe.requireInteractiveContext().editor.pendingImages.length;
  if (pointers.some(parts => parts[0] === "forcedTool")) observation.forcedTool = probe.forcedToolChoice();
  if (pointers.some(parts => parts[0] === "pinned")) {
    const pins = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/session/session-pins")>("packages/coding-agent/src/session/session-pins.ts");
    await ownedPath(state, join(session.settings.getAgentDir(), "session-pins.json"));
    observation.pinned = (await pins.loadPinnedSessionIds(session.settings.getAgentDir())).has(manager.getSessionId());
  }
  if (pointers.some(parts => parts[0] === "handoffDocumentExists" || parts[0] === "handoffDocument")) {
    const directory = manager.getArtifactsDir();
    let names: string[] = [];
    if (directory) {
      try { names = await readdir(await ownedPath(state, directory)); }
      catch (error) { if (!isMissing(error)) throw error; }
    }
    observation.handoffDocumentExists = false;
    for (const name of names) {
      if (!name.startsWith("handoff-") || !name.endsWith(".md") || !directory) continue;
      const document = await readOwnedText(state, join(directory, name));
      if (document.trim().length > 0) { observation.handoffDocumentExists = true; observation.handoffDocument = document; break; }
    }
  }
  Object.assign(observation, await captureSessionWorkflows(state, session, manifest), await capturePlanWorkflows(state, session));
  if (pointers.some(parts => parts[0] === "vibe")) observation.vibe = await captureVibe(state, session, probe.requireToolSession());
  return observation;
}

export async function captureBtw(state: CaptureState, session: AgentSession): Promise<unknown> {
  await probe.requireBtwController().flush();
  const directory = session.sessionManager.getArtifactsDir();
  if (!directory) throw new Error("BTW capture requires the actual persisted artifacts directory");
  await ownedPath(state, directory);
  const module = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/session/btw-history")>("packages/coding-agent/src/session/btw-history.ts");
  const scope = probe.requireInteractiveContext().focusedAgentId ? session.sessionManager.getSessionId() : undefined;
  const store = await module.BtwHistoryStore.open(directory, scope);
  return store.getRecords().flatMap(record => module.getBtwTurns(record));
}

export async function captureBackground(state: CaptureState, session: AgentSession): Promise<unknown> {
  await session.settleAsyncWork();
  const snapshot = state.context.extensionContext.getAsyncJobSnapshot();
  const tree = session.getAsyncJobTree({ recentLimit: Number.MAX_SAFE_INTEGER });
  if (!snapshot || !tree) throw new Error("Missing actual async job manager for background capture");
  const job = tree.recent[0] ?? tree.running[0];
  if (!job) throw new Error("No actual background task observed");
  return { ...job, snapshot, tree, inspection: session.inspectAsyncJob(job.id) };
}

export async function captureRecording(state: CaptureState): Promise<unknown> {
  const recorder = probe.recordingState();
  const module = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/stream/recording")>("packages/coding-agent/src/stream/recording.ts");
  const present = await exists(state, recorder.path);
  const result: Record<string, unknown> = { ...recorder, exists: present };
  if (present && !recorder.active) {
    const recording = module.parseRecording(await readOwnedText(state, recorder.path));
    result.hasFrames = recording.events.length > 0;
    result.recording = recording;
  }
  return result;
}
