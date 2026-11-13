import * as fs from "node:fs";
import { writeFile } from "node:fs/promises";
import type * as AgentHub from "@omp-source/packages/tui/src/overlays/agent-hub";
import type * as Observers from "@omp-source/packages/tui/src/overlays/session-observer-registry";
import type { AgentProgress } from "@omp-source/packages/tui/src/tools/task";
import type { AgentRecordLike } from "@omp-source/packages/tui/src/overlays/agent-hub-types";
import type { AgentTranscriptEntry } from "@omp-source/packages/tui/src/overlays/agent-transcript-viewer";
import { callbackValue, visibleText, type FixtureCase } from "../scenario";

export const fixtureCases: FixtureCase[] = [{
  id: "app-agent-hub-transcript-return-and-release",
  covers: ["component:packages/tui/src/overlays/agent-hub.ts#AgentHubOverlayComponent", "component:packages/tui/src/overlays/agent-transcript-viewer.ts#AgentTranscriptViewer", "component:packages/tui/src/chat/transcript-browser.ts#TranscriptBrowser"],
  async create(context) {
    const { AgentHubOverlayComponent } = await context.load<typeof AgentHub>("packages/tui/src/overlays/agent-hub.ts");
    const { SessionObserverRegistry } = await context.load<typeof Observers>("packages/tui/src/overlays/session-observer-registry.ts");
    const entry: AgentTranscriptEntry = { type: "message", id: "fixture-message", parentId: null, timestamp: "2026-11-01T12:00:00Z", message: { role: "user", content: "Inspect the fixture acknowledgement", timestamp: 1 } };
    const sessionFile = `${context.workspace}/fixture-agent.jsonl`;
    await writeFile(sessionFile, JSON.stringify(entry) + "\n");
    const agent: AgentRecordLike = { id: "FixtureScout", displayName: "FixtureScout", kind: "sub", parentId: "Main", status: "parked", session: null, sessionFile, createdAt: 1794000000000, lastActivity: 1794000000000, activity: "Inspect fixture acknowledgement" };
    const listeners = new Set<() => void>();
    const observers = new SessionObserverRegistry();
    const channels = new Map<string, Set<(data: unknown) => void>>();
    const bus = { on: (channel: string, listener: (data: unknown) => void) => { const set = channels.get(channel) ?? new Set(); set.add(listener); channels.set(channel, set); return () => { set.delete(listener); }; } };
    observers.subscribeToEventBus(bus, bus);
    const progress: AgentProgress = { index: 0, id: agent.id, agent: "fixture", agentSource: "project", status: "running", task: "Inspect fixture acknowledgement", recentTools: [], recentOutput: [], toolCount: 3, requests: 2, tokens: 5200, contextTokens: 4096, contextWindow: 32000, cost: 0.42, durationMs: 90000 };
    for (const listener of channels.get("task:subagent:progress") ?? []) listener({ index: 0, agent: "fixture", agentSource: "project", task: progress.task, progress, sessionFile });
    const component = new AgentHubOverlayComponent({
      ui: context.tui, cwd: context.workspace, observers,
      registry: { list: () => [agent], get: id => id === agent.id ? agent : undefined, onChange: listener => { listeners.add(listener); return () => { listeners.delete(listener); }; } },
      transcript: { fs, parseEntries: text => text.split("\n").filter(Boolean).map(line => { const parsed: AgentTranscriptEntry = JSON.parse(line); return parsed; }) },
      loadPersisted: async () => undefined, hubKeys: [], requestRender: () => context.tui.requestRender(),
      irc: { unreadCount: () => 0 }, manageActivityLive: false,
      activity: { setLive: () => undefined, sync: async () => undefined, query: () => [], recent: () => [] },
      lifecycle: () => ({ ensureLive: async () => { throw new Error("Fixture forbids agent execution"); }, release: async (id, expected, options) => { if (expected !== agent) throw new Error("Unexpected agent ownership"); agent.status = "aborted"; await writeFile(`${context.workspace}/agent-state.json`, JSON.stringify({ id, status: agent.status, tombstone: options.tombstone })); context.record("release", { id, tombstone: options.tombstone }); for (const listener of listeners) listener(); return true; } }),
      onDone: () => { component.dispose(); context.record("close", true); context.done(); },
    });
    await component.persistedSubagentsReady;
    return component;
  },
  checkpoints: [
    { name: "agent roster", comparisons: [visibleText("agent", "(FixtureScout)", '[data-omp-picker-item] :text-is("FixtureScout")', "FixtureScout")] },
    { name: "open real transcript overlay", terminalInput: "\r", browserActions: [{ type: "click", selector: 'button:has-text("Open transcript")' }], comparisons: [visibleText("context usage", "(12\\.8%/32K)", ':text-is("12.8%/32K")', "12.8%/32K"), visibleText("saved prompt", "(Inspect the fixture acknowledgement)", 'text="Inspect the fixture acknowledgement"', "Inspect the fixture acknowledgement")] },
    { name: "send to parked agent reports its refusal", terminalInput: "hello\r", waitForMilliseconds: 500, browserActions: [{ type: "fill", selector: '[data-omp-role="omp.field"] textarea', value: "hello" }, { type: "press", key: "Enter", selector: '[data-omp-role="omp.field"] textarea' }], comparisons: [visibleText("notice", "(Fixture forbids agent execution)", 'text="Fixture forbids agent execution"', "Fixture forbids agent execution")] },
    { name: "close button returns only to owner", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button[title^="Close"] >> nth=-1' }], comparisons: [visibleText("roster restored", "(FixtureScout)", '[data-omp-picker-item] :text-is("FixtureScout")', "FixtureScout")] },
    { name: "release selected agent", terminalInput: "x", browserActions: [{ type: "click", selector: 'button:has-text("Kill")' }], comparisons: [callbackValue("lifecycle release", "release", { id: "FixtureScout", tombstone: true }), { name: "persisted abort", terminal: { type: "file", path: "agent-state.json", pointer: "/status" }, browser: { type: "file", path: "agent-state.json", pointer: "/status" }, expected: "aborted" }] },
    { name: "close owner", terminalInput: "\x1b", browserActions: [{ type: "press", key: "Escape" }], comparisons: [callbackValue("closed", "close", true)] },
  ],
}];
