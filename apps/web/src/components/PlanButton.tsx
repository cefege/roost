// PlanButton — M3 FAB shown ONLY on agent sessions at idle (gated by the
// caller via liveStatus(props.session) === "idle"). Tap → types "/plan" + CR
// into the PTY, entering plan mode in whatever agent is running (claude, pi,
// omp, … — any agent Roost tracks via screen-scrape). Same PTY-input path as
// the agent-launch + mic FABs (inputChannel.sendInput), and shares the
// agent-launch button's fixed slot (the two are mutually exclusive: agent-launch
// shows only on shells at a shell prompt). Mounted by CellTerminal.tsx.

import type { Component } from "solid-js";
import { inputChannel } from "../ws/input-channel.ts";
import { onFabPointerDown } from "../lib/fabDragOffset.ts";

export const PlanButton: Component<{ sessionId: string }> = (props) => (
  <button
    type="button"
    class="plan-fab"
    data-testid="plan-shortcut"
    aria-label="Enter plan mode — types '/plan' and runs it in this agent"
    title="Plan mode"
    onPointerDown={onFabPointerDown}
    onClick={() => inputChannel.sendInput(props.sessionId, new TextEncoder().encode("/plan\r"))}
  >
    <span class="plan-fab__label">/plan</span>
  </button>
);
