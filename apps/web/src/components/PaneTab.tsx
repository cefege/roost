/*
 * One presentational terminal tab for PaneStrip.
 * PaneStrip owns drag, close, and selection state; this component maps that state
 * to the stable tab DOM and its accessible controls.
 */

import { Show, type JSX } from "solid-js";
import type { Session } from "@roost/protocol/wire";
import { sessionTitle } from "../lib/sessionTitle.ts";
import { sessionTerminalTransportKind, sessionTerminalTransportLabel } from "../store/local-transport-indicator.ts";
import { notifyTargetSessionId } from "../store/notifyTarget.ts";
import { Button } from "./Settings/md/Button.tsx";
import { IconButton } from "./Settings/md/IconButton.tsx";
import { Icon } from "./Settings/md/Icon.tsx";
import { AgentStatusIndicator } from "./AgentStatusIndicator.tsx";
import { hoverCardAvailable } from "./PaneTabHoverCard.tsx";
export interface PaneTabProps {
  session: Session;
  active: boolean;
  dragging: boolean;
  closing: boolean;
  style: JSX.CSSProperties;
  onPointerDown: (event: PointerEvent) => void;
  onSelect: () => void;
  onHoverStart: (element: HTMLElement) => void;
  onHoverEnd: () => void;
  onClose: (event: MouseEvent) => void;
}

export function PaneTab(props: PaneTabProps) {
  let tabElement: HTMLDivElement | undefined;
  const transportKind = () => sessionTerminalTransportKind(props.session.id);
  const directTooltip = () => sessionTerminalTransportLabel(props.session.id);

  // The hover card and the OS tooltip would otherwise stack on a plain desktop:
  // the tooltip is the fallback for surfaces that get no hover card at all.
  const nativeTooltip = () => {
    if (hoverCardAvailable()) return undefined;
    const title = sessionTitle(props.session);
    const carrier = directTooltip();
    return carrier ? `${title} — ${carrier}` : title;
  };

  return (
    <div
      ref={tabElement}
      class="df-tab workbench-pane-tab"
      data-testid={`tab-${props.session.id}`}
      data-active={props.active ? "true" : "false"}
      data-dragging={props.dragging ? "true" : "false"}
      data-closing={props.closing ? "true" : "false"}
      data-terminal-transport={transportKind() ?? undefined}
      data-notify-target={notifyTargetSessionId() === props.session.id ? "true" : undefined}
      style={props.style}
      onMouseEnter={() => {
        if (tabElement) props.onHoverStart(tabElement);
      }}
      onMouseLeave={props.onHoverEnd}
    >
      <Button
        variant="ghost"
        class="workbench-pane-tab__select"
        aria-label={sessionTitle(props.session)}
        aria-current={props.active ? "page" : undefined}
        onPointerDown={props.onPointerDown}
        onClick={props.onSelect}
        title={nativeTooltip()}
      >
        <Icon name="terminal" size="sm" class="workbench-pane-tab__icon" />
        {/* Left of the label with the terminal mark: this is a property of the
            tab, and sitting right of it would read as a second live status
            beside AgentStatusIndicator. */}
        <Show when={directTooltip()}>
          <Icon name="bolt" size="sm" class="workbench-pane-tab__local" />
        </Show>
        <span class="df-tab-label workbench-pane-tab__label">{sessionTitle(props.session)}</span>
        <AgentStatusIndicator
          sessionId={props.session.id}
          compact
          suppressTooltip={hoverCardAvailable()}
        />
      </Button>
      <IconButton
        icon="close"
        label="Close terminal"
        size="icon-sm"
        class="df-tab-close workbench-pane-tab__close"
        data-testid={`tab-close-${props.session.id}`}
        onClick={props.onClose}
      />
    </div>
  );
}

