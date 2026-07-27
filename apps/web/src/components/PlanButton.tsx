// PlanButton — M3 FAB shown ONLY on agent sessions at idle (gated by the
// caller via liveStatus(props.session) === "idle"). Tap → types "/plan" + CR
// into the PTY for an agent that supports it. OMP structured state drives the
// visible lifecycle; terminal input remains the command path.

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
