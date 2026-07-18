// CostChip — monospace dollar-amount pill for a session row.
// Hidden when cost_usd < $0.01 or absent. Reads session.agent.cost_usd.
// Callers: SessionRow.
// Depends on: @roost/shared/wire Session type, Settings/md/primitives Chip.

import type { Component } from "solid-js";
import { Show } from "solid-js";
import type { Session } from "@roost/shared/wire";
import { Chip } from "../Settings/md/primitives.tsx";

interface Props {
  session: Session;
}

function formatCostUsd(cost: number | null | undefined): string | null {
  if (cost == null || !Number.isFinite(cost) || cost < 0.01) return null;
  return `$${cost.toFixed(2)}`;
}

export const CostChip: Component<Props> = (props) => {
  const text = () => formatCostUsd(props.session.agent?.cost_usd);
  return (
    <Show when={text()}>
      <Chip label={text()!} testId="cost-chip" />
    </Show>
  );
};
