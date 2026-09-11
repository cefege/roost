/*
 * Desktop-only hover card for a terminal tab.
 * PaneStrip determines dwell timing and which session is hovered; this component
 * renders the read-only title, process context, and live preview surface.
 */

import { Show, createMemo, createSignal, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import type { Session } from "@roost/shared/wire";
import { programSubtitle, sessionTitle } from "../lib/sessionTitle.ts";
import { shortCwd } from "../lib/sidebarFormat.ts";
import { renderPreview } from "../lib/terminalPreview.ts";
import { AgentStatusIndicator } from "./AgentStatusIndicator.tsx";
import { Icon, Surface } from "./Settings/md/primitives.tsx";

export interface PaneTabHoverCardProps {
  session: Session;
  rect: DOMRect;
}

export function PaneTabHoverCard(props: PaneTabHoverCardProps) {
  let previewElement: HTMLDivElement | undefined;
  const [hasPreview, setHasPreview] = createSignal(false);
  const subtitle = createMemo(() => programSubtitle(props.session));
  const left = `max(var(--md-space-2), min(${props.rect.left}px, calc(100vw - var(--workbench-tab-hovercard-width) - var(--md-space-2))))`;

  onMount(() => {
    if (previewElement) setHasPreview(renderPreview(props.session.id, previewElement));
  });

  return (
    <Portal>
      <Surface
        level={3}
        elevation={3}
        radius="md"
        class="df-tab-hovercard"
        data-testid="tab-hovercard"
        style={{
          left,
          top: `calc(${props.rect.bottom}px + var(--md-space-2))`,
        }}
      >
        <div class="df-tab-hovercard-head">
          <Icon name="terminal" size="sm" />
          <span class="df-tab-hovercard-title">
            {sessionTitle(props.session)}
          </span>
          <AgentStatusIndicator sessionId={props.session.id} />
        </div>
        <Show when={subtitle()}>
          <div class="df-tab-hovercard-line">{subtitle()}</div>
        </Show>
        <div class="df-tab-hovercard-cwd">
          {shortCwd(props.session.cwd, props.session.worker_fp)}
        </div>
        <div
          class="df-tab-hovercard-preview"
          data-preview={hasPreview() ? "true" : "false"}
        >
          <div ref={previewElement} class="terminal-card-preview-text" />
        </div>
      </Surface>
    </Portal>
  );
}
