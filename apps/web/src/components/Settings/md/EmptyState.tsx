// Shared empty-state content owns its layout and type styles on every route.
// Search, file, browse, and settings surfaces compose it with caller-owned actions.
// EmptyState.css travels with this component instead of the lazy Settings shell.

import { type JSX, type Component, Show } from "solid-js";
import { Icon } from "./Icon.tsx";
import "./EmptyState.css";

export const EmptyState: Component<{
  icon: string;
  title: string;
  supporting?: string;
  action?: JSX.Element;
}> = (props) => (
  <div class="md-empty-state">
    <Icon name={props.icon} size="lg" class="md-empty-state__icon" />
    <div class="md-empty-state__title">{props.title}</div>
    <Show when={props.supporting}>
      <div class="md-empty-state__supporting">{props.supporting}</div>
    </Show>
    <Show when={props.action}>{props.action}</Show>
  </div>
);
