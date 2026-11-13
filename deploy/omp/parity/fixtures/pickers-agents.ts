//! Agent roster, agent library and hook choice proof scenarios: the real overlays over fixture agents and
//! options, proving the picker properties their describe hooks emit. Registered by pickers.ts.
import * as fs from "node:fs";
import type * as AgentHub from "@omp-source/packages/tui/src/overlays/agent-hub";
import type * as AgentsHub from "@omp-source/packages/tui/src/overlays/agents-hub";
import type * as HookSelector from "@omp-source/packages/tui/src/overlays/hook-selector";
import type * as Observers from "@omp-source/packages/tui/src/overlays/session-observer-registry";
import type { AgentRecordLike } from "@omp-source/packages/tui/src/overlays/agent-hub-types";
import type { ModelBrowserSource } from "@omp-source/packages/tui/src/overlays/model-browser";
import type { FixtureCase } from "../scenario";
import { pickerClaims } from "./pickers-claims-agents";
import { below, coversFor, literal } from "./pickers-support";

const overlays = "packages/tui/src/overlays/";
const roster = pickerClaims["agent-roster"]!;
const tree = pickerClaims["agent-tree"]!;
const library = pickerClaims["agents-library"]!;
const options = pickerClaims["hook-options"]!;
const browserSource: ModelBrowserSource = {
  revision: 0, defaultThinkingLevel: "off", modelProviderOrder: [], knownRoleIds: [], mruOrder: [], modelPerf: new Map(),
  getRoleInfo: role => ({ name: role, section: "chat", accepts: () => true }), getModelRole: () => undefined,
  defaultRoleChain: () => [], resolveRoleValue: () => ({ model: undefined, explicitThinkingLevel: false }),
};
const agentHeading = { terminal: "Agent Hub", browser: 'h2:text-is("Agent Hub")' };
const libraryHeading = { terminal: "Agents", browser: 'h2:text-is("Agents")' };
const hookHeading = { terminal: "Fixture hook choices", browser: 'h2:text-is("Fixture hook choices")' };
const childRow = { terminal: "FixtureChild", browser: '[data-omp-picker-item="FixtureChild"]' };

export const fixtureCases: FixtureCase[] = [
  {
    id: "picker-agent-roster",
    covers: coversFor(roster, tree),
    async create(context) {
      const { AgentHubOverlayComponent } = await context.load<typeof AgentHub>(`${overlays}agent-hub.ts`);
      const { SessionObserverRegistry } = await context.load<typeof Observers>(`${overlays}session-observer-registry.ts`);
      const make = (id: string, status: AgentRecordLike["status"], parentId: string): AgentRecordLike => ({ id, displayName: id, kind: "sub", parentId, status, session: null, sessionFile: `${context.workspace}/${id}.jsonl`, createdAt: 1794000000000, lastActivity: 1794000000000, activity: `Inspect ${id}`, ...(id === "FixtureChild" ? { history: { resolvedModel: "parity/fixture-coder:high" } } : {}) });
      const agents = [make("FixtureScout", "parked", "Main"), make("FixtureChild", "aborted", "FixtureScout")];
      const component = new AgentHubOverlayComponent({
        ui: context.tui, cwd: context.workspace, observers: new SessionObserverRegistry(),
        registry: { list: () => agents, get: id => agents.find(agent => agent.id === id), onChange: () => () => undefined },
        transcript: { fs, parseEntries: () => [] },
        loadPersisted: async () => undefined, hubKeys: [], requestRender: () => context.tui.requestRender(),
        irc: { unreadCount: id => id === "FixtureScout" ? 2 : 0 }, manageActivityLive: false,
        activity: { setLive: () => undefined, sync: async () => undefined, query: () => [], recent: () => [] },
        lifecycle: () => ({ ensureLive: async () => { throw new Error("no"); }, release: async () => true }),
        onDone: () => { component.dispose(); context.done(); },
      });
      await component.persistedSubagentsReady;
      return component;
    },
    checkpoints: [
      { name: "roster", comparisons: [literal("agent", "(FixtureScout)", { selector: '[data-omp-picker-item] :text-is("FixtureScout")' }, "FixtureScout", []), below("roster properties", childRow, agentHeading, roster)] },
      { name: "tree view", terminalInput: "t", browserActions: [{ type: "click", selector: 'button:has-text("By parent")' }], comparisons: [below("tree properties", childRow, agentHeading, tree)] },
    ],
  },
  {
    id: "picker-agent-library",
    covers: coversFor(library),
    async create(context) {
      const { AgentsHubComponent } = await context.load<typeof AgentsHub>(`${overlays}agents-hub.ts`);
      const agent: AgentsHub.HubAgent = { name: "FixtureScout", description: "Read only fixture research", systemPrompt: "Inspect fixture data", source: "project", disabled: false };
      const second: AgentsHub.HubAgent = { name: "FixtureOff", description: "Disabled fixture agent", systemPrompt: "Idle", source: "user", disabled: true };
      return AgentsHubComponent.create(context.tui, {
        browserSource, loadAgents: async () => [agent, second], getAvailableModels: () => [], effectiveModelPatterns: () => [], resolvePatterns: () => undefined,
        effectivePrewalkPattern: () => undefined, effectiveAdvisorPattern: () => undefined,
        setAgentDisabled: () => undefined, setAgentOverride: () => undefined,
        generateAgent: async () => { throw new Error("no"); }, saveAgent: async () => { throw new Error("no"); },
      }, { onCancel: () => context.done() });
    },
    checkpoints: [{
      name: "library",
      comparisons: [
        literal("agent", "(FixtureScout)", { selector: '[data-omp-picker-item] :text-is("FixtureScout")' }, "FixtureScout", []),
        below("library properties", { terminal: "FixtureOff", browser: '[data-omp-picker-item="agent:user:FixtureOff"]' }, libraryHeading, library),
      ],
    }],
  },
  {
    id: "picker-hook-options",
    covers: coversFor(options),
    async create(context) {
      const { HookSelectorComponent } = await context.load<typeof HookSelector>(`${overlays}hook-selector.ts`);
      return new HookSelectorComponent("Fixture hook choices", [{ label: "First option", description: "first detail" }, "Second option", "Third option", "Fourth option", "Fifth option", "Sixth option", "Seventh option"], () => context.done(), () => context.done(), { selectionMarker: "checkbox", checkedIndices: [1], disabledIndices: [2] });
    },
    checkpoints: [{
      name: "options",
      comparisons: [
        literal("option", "(First option)", { selector: '[data-omp-picker-item="0"] :text-is("First option")' }, "First option", []),
        below("option properties", { terminal: "Fourth option", browser: '[data-omp-picker-item="3"]' }, hookHeading, options),
      ],
    }],
  },
];
