// Browser agent-launch configuration cached for the confirmed dashboard.
// Startup and settings load it from the coordinator while launchers read signals.
// A dashboard boundary resets defaults and fences late responses from the old scope.

import { createSignal } from "solid-js";
import { coordClient } from "../connect.ts";

export interface AgentDef {
  id: string;
  label: string;
  command: string;
  color: string;
  glyph: string;
}

export const BUILTIN_AGENTS: AgentDef[] = [
  { id: "codex", label: "OpenAI Codex", command: "codex", color: "#10A37F", glyph: "Cx" },
  { id: "gemini", label: "Gemini CLI", command: "gemini", color: "#1A73E8", glyph: "G" },
  { id: "opencode", label: "OpenCode", command: "opencode", color: "#F59E0B", glyph: "OC" },
  { id: "cursor", label: "Cursor Agent", command: "cursor-agent", color: "#6E56CF", glyph: "Cu" },
  { id: "amp", label: "Amp", command: "amp", color: "#E5484D", glyph: "A" },
  { id: "copilot", label: "GitHub Copilot CLI", command: "copilot", color: "#6E7681", glyph: "Co" },
  { id: "droid", label: "Droid", command: "droid", color: "#7C3AED", glyph: "D" },
  { id: "grok", label: "Grok CLI", command: "grok", color: "#111827", glyph: "Gr" },
  { id: "pi", label: "Pi", command: "pi", color: "#16A34A", glyph: "π" },
  { id: "omp", label: "OMP", command: "omp", color: "#8B5CF6", glyph: "O" },
];

const AGENTS_BY_ID: Record<string, AgentDef> = Object.fromEntries(
  BUILTIN_AGENTS.map((agent) => [agent.id, agent]),
);
const DEFAULT_AGENT = AGENTS_BY_ID["omp"]!;
const CUSTOM_COLOR = "#6E7681";

export interface ResolvedAgent {
  id: string;
  label: string;
  command: string;
  color: string;
  glyph: string;
  isCustom: boolean;
}

export function resolveAgentFrom(selected: string, custom: string): ResolvedAgent {
  if (selected === "custom") {
    const command = custom.trim();
    if (!command) return { ...DEFAULT_AGENT, isCustom: false };
    return {
      id: "custom",
      label: "Custom",
      command,
      color: CUSTOM_COLOR,
      glyph: command[0]!.toUpperCase(),
      isCustom: true,
    };
  }
  const definition = AGENTS_BY_ID[selected] ?? DEFAULT_AGENT;
  return { ...definition, isCustom: false };
}

const [selectedAgent, setSelectedAgent] = createSignal("omp");
const [customCommand, setCustomCommand] = createSignal("");
const [autoLaunch, setAutoLaunch] = createSignal(false);
export const currentSelected = selectedAgent;
export const currentCustomCommand = customCommand;
export const autoLaunchEnabled = autoLaunch;
export const resolveAgent = (): ResolvedAgent =>
  resolveAgentFrom(selectedAgent(), customCommand());

let configGeneration = 0;
let configRequest: Promise<void> | null = null;

export function clearAgentConfigForDashboardSwitch(): void {
  configGeneration++;
  configRequest = null;
  setSelectedAgent("omp");
  setCustomCommand("");
  setAutoLaunch(false);
}

/** Load once per dashboard generation and discard a response from an old scope. */
export async function loadAgentConfig(): Promise<void> {
  if (configRequest) return configRequest;
  const requestGeneration = configGeneration;
  let request: Promise<void>;
  request = coordClient.agentConfigGet({}).then((config) => {
    if (requestGeneration !== configGeneration) return;
    setSelectedAgent(config.selected || "omp");
    setCustomCommand(config.customCommand ?? "");
    setAutoLaunch(config.autoLaunch ?? false);
  }).catch(() => {
    // First paint, pre-authorization, and offline startup retain safe defaults.
  }).finally(() => {
    if (configRequest === request) configRequest = null;
  });
  configRequest = request;
  return request;
}

export async function saveAgentConfig(selected: string, command: string): Promise<void> {
  const requestGeneration = configGeneration;
  await coordClient.agentConfigSet({
    selected,
    customCommand: command,
    autoLaunch: autoLaunch(),
  });
  if (requestGeneration !== configGeneration) return;
  setSelectedAgent(selected);
  setCustomCommand(command);
}

export async function saveAutoLaunch(enabled: boolean): Promise<void> {
  const requestGeneration = configGeneration;
  await coordClient.agentConfigSet({
    selected: selectedAgent(),
    customCommand: customCommand(),
    autoLaunch: enabled,
  });
  if (requestGeneration === configGeneration) setAutoLaunch(enabled);
}
