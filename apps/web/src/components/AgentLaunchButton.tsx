// AgentLaunchButton — M3 FAB shown ONLY on shell sessions (gated by the
// caller via session.kind === "shell"). Tap → types the selected agent's
// command + CR into the PTY, launching it in the shell. Same PTY-input path
// as the mic (inputChannel.sendInput), and modeled on MobileVoiceInput's
// bottom-right floating FAB. Mounted by CellTerminal.tsx; sits left of the mic
// FAB. The agent is configurable in Settings → Agents → Launcher.

import type { Component } from "solid-js";
import { inputChannel } from "../ws/input-channel.ts";
import { onFabPointerDown } from "../lib/fabDragOffset.ts";
import { resolveAgent } from "../lib/agents.ts";
import { AgentGlyph } from "./AgentGlyph.tsx";

export const AgentLaunchButton: Component<{ sessionId: string }> = (props) => {
  const agent = () => resolveAgent();
  return (
    <button
      type="button"
      class="agent-launch-fab"
      data-testid="agent-launch"
      aria-label={`Launch ${agent().label} — types '${agent().command}' and runs it in this shell`}
      title={`Launch ${agent().label} in this shell`}
      onPointerDown={onFabPointerDown}
      onClick={() => inputChannel.sendInput(props.sessionId, new TextEncoder().encode(agent().command + "\r"))}
    >
      <span class="agent-launch-fab__icon"><AgentGlyph agent={agent()} /></span>
    </button>
  );
};
