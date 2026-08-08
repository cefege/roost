import type { Component } from "solid-js";
import { Show } from "solid-js";
import { Dynamic } from "solid-js/web";
import { AGENT_MARKS } from "./AgentMarks.tsx";
import type { ResolvedAgent } from "../lib/agents.ts";

/** Settings preview — colored brand tile. `size` px square. */
export const AgentTile: Component<{ agent: ResolvedAgent; size?: number }> = (props) => {
  const s = () => props.size ?? 32;
  return (
    <span style={{
      width: `${s()}px`, height: `${s()}px`, "border-radius": "9px", flex: "0 0 auto",
      display: "grid", "place-items": "center", background: props.agent.color, color: "var(--ansi-bright-white)",
      "font-weight": "700", "font-size": `${Math.round(s() * 0.42)}px`, "line-height": "1",
    }}>
      <Show when={AGENT_MARKS[props.agent.id]} fallback={<>{props.agent.glyph}</>}>
        {(mark) => <Dynamic component={mark()} />}
      </Show>
    </span>
  );
};
