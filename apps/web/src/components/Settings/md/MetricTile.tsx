import { type Component, Show } from "solid-js";
import { Icon } from "./Icon.tsx";

// ─── Metric tile (CPU / Mem / Disk / Bandwidth) ────────────────────
export const MetricTile: Component<{
  label: string;
  icon?: string;
  value: string;
  support?: string;
  /** 0..1 — renders a progress bar. Omit to skip the bar. */
  ratio?: number;
}> = (props) => (
  <div class="md-metric-tile">
    <span class="md-metric-tile__label">
      <Show when={props.icon}>
        <Icon name={props.icon!} size="sm" />
      </Show>
      {props.label}
    </span>
    <span class="md-metric-tile__value">{props.value}</span>
    <Show when={props.support}>
      <span class="md-metric-tile__support">{props.support}</span>
    </Show>
    <Show when={typeof props.ratio === "number"}>
      <div class="md-metric-tile__bar">
        <span style={{ width: `${Math.min(100, Math.max(0, (props.ratio ?? 0) * 100))}%` }} />
      </div>
    </Show>
  </div>
);
