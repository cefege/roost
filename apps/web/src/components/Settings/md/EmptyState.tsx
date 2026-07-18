import { type JSX, type Component, Show } from "solid-js";
import { Icon } from "./Icon.tsx";

// ─── Empty state ───────────────────────────────────────────────────
export const EmptyState: Component<{
  icon: string;
  title: string;
  supporting?: string;
  action?: JSX.Element;
}> = (props) => (
  <div class="md-empty-state">
    <Icon name={props.icon} size="lg" class="md-empty-state__icon" />
    <div class="md-title-m" style={{ color: "var(--md-sys-color-on-surface)" }}>{props.title}</div>
    <Show when={props.supporting}>
      <div class="md-body-m" style={{ "max-width": "320px" }}>{props.supporting}</div>
    </Show>
    <Show when={props.action}>{props.action}</Show>
  </div>
);
