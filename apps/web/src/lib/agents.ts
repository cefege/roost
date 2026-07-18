import { createSignal } from "solid-js";
import { coordClient } from "../connect.ts";

export interface AgentDef {
  id: string; label: string; command: string; color: string; glyph: string;
}
// Curated main players. Launch command = the agent's own CLI binary.
// glyph = TEXT FALLBACK only — agents with an official mark render it from
// components/AgentMarks.tsx (keyed by id); glyph shows when no mark exists
// (custom commands) or one is ever removed.
export const BUILTIN_AGENTS: AgentDef[] = [
  { id: "claude",   label: "Claude Code",        command: "claude",        color: "#D97757", glyph: "" },
  { id: "codex",    label: "OpenAI Codex",       command: "codex",         color: "#10A37F", glyph: "Cx" },
  { id: "gemini",   label: "Gemini CLI",         command: "gemini",        color: "#1A73E8", glyph: "G"  },
  { id: "opencode", label: "OpenCode",           command: "opencode",      color: "#F59E0B", glyph: "OC" },
  { id: "cursor",   label: "Cursor Agent",       command: "cursor-agent",  color: "#6E56CF", glyph: "Cu" },
  { id: "amp",      label: "Amp",                command: "amp",           color: "#E5484D", glyph: "A"  },
  { id: "copilot",  label: "GitHub Copilot CLI", command: "copilot",       color: "#6E7681", glyph: "Co" },
  { id: "droid",    label: "Droid",              command: "droid",         color: "#7C3AED", glyph: "D"  },
  { id: "grok",     label: "Grok CLI",           command: "grok",          color: "#111827", glyph: "Gr" },
  { id: "pi",       label: "Pi",                 command: "pi",            color: "#16A34A", glyph: "π"  },
  { id: "omp",      label: "OMP",                command: "omp",           color: "#8B5CF6", glyph: "O"  },
];
const AGENTS_BY_ID: Record<string, AgentDef> = Object.fromEntries(BUILTIN_AGENTS.map(a => [a.id, a]));

export interface ResolvedAgent {
  id: string; label: string; command: string; color: string; glyph: string; isCustom: boolean;
}
const CLAUDE = AGENTS_BY_ID["claude"]!;
const CUSTOM_COLOR = "#6E7681";

/** Pure: (stored choice) → effective agent. Unknown id → claude. Empty custom → claude. */
export function resolveAgentFrom(selected: string, custom: string): ResolvedAgent {
  if (selected === "custom") {
    const cmd = custom.trim();
    if (!cmd) return { ...CLAUDE, isCustom: false };
    return { id: "custom", label: "Custom", command: cmd, color: CUSTOM_COLOR,
             glyph: cmd[0]!.toUpperCase(), isCustom: true };
  }
  const def = AGENTS_BY_ID[selected] ?? CLAUDE;
  return { ...def, isCustom: false };
}

const [selectedSig, setSelectedSig] = createSignal("claude");
const [customSig, setCustomSig] = createSignal("");
const [autoLaunchSig, setAutoLaunchSig] = createSignal(false);
export const currentSelected = selectedSig;
export const currentCustomCommand = customSig;
export const autoLaunchEnabled = autoLaunchSig;
export const resolveAgent = (): ResolvedAgent => resolveAgentFrom(selectedSig(), customSig());

/** Load once on boot (fire-and-forget). */
export async function loadAgentConfig(): Promise<void> {
  try {
    const c = await coordClient.agentConfigGet({});
    setSelectedSig(c.selected || "claude");
    setCustomSig(c.customCommand ?? "");
    setAutoLaunchSig(c.autoLaunch ?? false);
  } catch { /* first-paint before auth / offline: keep claude default */ }
}
/** Persist + update local signals so this device reflects immediately. */
export async function saveAgentConfig(selected: string, customCommand: string): Promise<void> {
  await coordClient.agentConfigSet({ selected, customCommand, autoLaunch: autoLaunchSig() });
  setSelectedSig(selected); setCustomSig(customCommand);
}
/** Persist auto-launch toggle immediately. */
export async function saveAutoLaunch(enabled: boolean): Promise<void> {
  await coordClient.agentConfigSet({
    selected: selectedSig(),
    customCommand: customSig(),
    autoLaunch: enabled,
  });
  setAutoLaunchSig(enabled);
}
