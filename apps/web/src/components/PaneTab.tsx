/*
 * One presentational terminal tab for PaneStrip.
 * PaneStrip owns drag, close, and selection state; this component maps that state
 * to the stable tab DOM and its accessible controls.
 */

import type { JSX } from "solid-js";
import type { Session } from "@roost/shared/wire";
import { sessionTitle } from "../lib/sessionTitle.ts";
import { Button } from "./Settings/md/Button.tsx";
import { IconButton } from "./Settings/md/IconButton.tsx";
import { Icon } from "./Settings/md/Icon.tsx";

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

  return (
    <div
      ref={tabElement}
      class="df-tab workbench-pane-tab"
      data-testid={`tab-${props.session.id}`}
      data-active={props.active ? "true" : "false"}
      data-dragging={props.dragging ? "true" : "false"}
      data-closing={props.closing ? "true" : "false"}
      style={props.style}
      onMouseEnter={() => {
        if (tabElement) props.onHoverStart(tabElement);
      }}
      onMouseLeave={props.onHoverEnd}
    >
      <Button
        variant="ghost"
        class="workbench-pane-tab__select"
        aria-current={props.active ? "page" : undefined}
        onPointerDown={props.onPointerDown}
        onClick={props.onSelect}
        title={sessionTitle(props.session)}
      >
        <Icon name="terminal" size="sm" class="workbench-pane-tab__icon" />
        <span class="df-tab-label workbench-pane-tab__label">{sessionTitle(props.session)}</span>
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

