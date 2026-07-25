// TerminalStatusBadge — per-terminal status tag. Renders as a non-interactive
// flex child of CellTerminal's `.term-pane-corner` top-right slot (shared with
// the omp chat/terminal toggle): the slot owns the positioning, this owns the
// pill. Colored by the agent's live attention level (Working / Needs input /
// Done / Idle) via the ONE vocab in lib/agentStatus.ts, so its color matches
// the pane's tab-strip dot for the same session. Claude/agent sessions only;
// hidden while the level is unknown. Plain shells render nothing. Caller:
// CellTerminal.tsx (gated !pending && !offline && inLayout — parked off-screen
// panes skip the attention recompute).
import { Show, createMemo, type Component } from "solid-js";
import type { Session } from "@roost/shared/wire";
import { attentionOf, presentationOf } from "../lib/agentStatus.ts";
import { isClaudeSession } from "../lib/isClaudeSession.ts";

export const TerminalStatusBadge: Component<{ session: Session }> = (props) => {
  const level = createMemo(() => attentionOf(props.session));
  const vis = createMemo(() => presentationOf(level()));
  const show = createMemo(() => isClaudeSession(props.session) && level() !== "unknown");
  return (
    <Show when={show()}>
      <span
        class="term-status-badge"
        data-testid={`terminal-status-badge-${props.session.id}`}
        data-level={level()}
        aria-label={vis().label}
        style={{ "--badge-color": vis().color }}
      >
        <span class="term-status-badge__dot" />
        {vis().short}
      </span>
    </Show>
  );
};
