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

const HOVER_CARD_WIDTH = 336;
const VIEWPORT_MARGIN = 8;

export interface PaneTabHoverCardProps {
  session: Session;
  rect: DOMRect;
}

export function PaneTabHoverCard(props: PaneTabHoverCardProps) {
  let previewElement: HTMLDivElement | undefined;
  const [hasPreview, setHasPreview] = createSignal(false);
  const subtitle = createMemo(() => programSubtitle(props.session));
  const left = Math.max(
    VIEWPORT_MARGIN,
    Math.min(props.rect.left, window.innerWidth - HOVER_CARD_WIDTH - VIEWPORT_MARGIN),
  );

  onMount(() => {
    if (previewElement) setHasPreview(renderPreview(props.session.id, previewElement));
  });

  return (
    <Portal>
      <Surface
        level={3}
        elevation={3}
        radius="md"
        class="df-tab-hovercard workbench-tab-hovercard"
        data-testid="tab-hovercard"
        style={{
          left: `${left}px`,
          top: `${props.rect.bottom + VIEWPORT_MARGIN}px`,
        }}
      >
        <div class="df-tab-hovercard-head workbench-tab-hovercard__head">
          <Icon name="terminal" class="workbench-tab-hovercard__icon" size="sm" />
          <span class="df-tab-hovercard-title workbench-tab-hovercard__title">
            {sessionTitle(props.session)}
          </span>
          <AgentStatusIndicator sessionId={props.session.id} />
        </div>
        <Show when={subtitle()}>
          <div class="df-tab-hovercard-line workbench-tab-hovercard__line">{subtitle()}</div>
        </Show>
        <div class="df-tab-hovercard-cwd workbench-tab-hovercard__cwd">
          {shortCwd(props.session.cwd, props.session.worker_fp)}
        </div>
        <div
          class="df-tab-hovercard-preview workbench-tab-hovercard__preview"
          data-preview={hasPreview() ? "true" : "false"}
        >
          <div ref={previewElement} class="terminal-card-preview-text" />
        </div>
      </Surface>
    </Portal>
  );
}
