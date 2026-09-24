import { createMemo, Show } from "solid-js";
import type { AgentStatus } from "@roost/protocol/wire";
import { rootStore } from "../../store/root.ts";
import { seenAgentRevision } from "../../lib/agentSeen.ts";
import {
  AGENT_STATUS_PRESENTATION,
  agentStatusTooltip,
  deriveAgentStatusLevel,
} from "../../client/agents/agentStatus.ts";
import { StatusDot } from "../Settings/md/StatusDot.tsx";

export function AgentStatusIndicator(props: {
  sessionId: string;
  compact?: boolean;
  /** Surfaces that own a richer hover affordance — the terminal tab, whose hover
   *  card carries this same status — suppress the dot's native tooltip so the OS
   *  tooltip cannot stack on top of that card. */
  suppressTooltip?: boolean;
  class?: string;
}) {
  const status = createMemo(
    () => rootStore.agent_status[props.sessionId] as AgentStatus | undefined,
  );
  const level = createMemo(() => {
    const current = status();
    return deriveAgentStatusLevel(current, seenAgentRevision(current));
  });
  const presentation = createMemo(() => AGENT_STATUS_PRESENTATION[level()]);

  return (
    <Show when={status()}>
      {(current) => (
        <Show when={level() !== "unknown"}>
          <span
            class={`agent-status ${props.compact ? "agent-status--compact" : ""} ${props.class ?? ""}`.trim()}
            data-testid={`agent-status-${props.sessionId}`}
            data-level={level()}
            title={props.suppressTooltip
              ? undefined
              : agentStatusTooltip(current(), seenAgentRevision(current()))}
            role="img"
            aria-label={presentation().label}
          >
            <StatusDot status={presentation().dotStatus} />
            <Show when={!props.compact}>
              <span class="agent-status__label">{presentation().label}</span>
            </Show>
          </span>
        </Show>
      )}
    </Show>
  );
}
