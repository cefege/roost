// Transfer popup — one M3 surface aggregates every upload and download.
// Rendered as a notification-dock child (NotificationDock.tsx owns placement and
// width) so jobs survive pane switches without claiming their own corner.
// Transfer lifecycle and per-job progress come from src/store/transfers.ts.
// Completed jobs preserve their existing dismissal policy; rows never create
// their own popup.

import { For, Show } from "solid-js";
import { List, Surface } from "./Settings/md/primitives.tsx";
import { TransferRow } from "./TransferRow.tsx";
import { transfers } from "../store/transfers.ts";

export function TransferStack() {
  const transferList = () => Object.values(transfers);
  return (
    <Show when={transferList().length > 0}>
      <Surface
        as="section"
        data-testid="transfer-card"
        aria-labelledby="transfer-popup-title"
        level={2}
        elevation={3}
        radius="md"
        pad={3}
        border
        style={{
          display: "flex",
          "flex-direction": "column",
          gap: "var(--md-space-2)",
          width: "100%",
        }}
      >
        <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}>
          <span
            id="transfer-popup-title"
            style={{ flex: "1", "font-size": "var(--md-title-s-size)", "font-weight": "var(--md-title-s-weight)" }}
          >
            Transfers
          </span>
          <span style={{ color: "var(--text-lo)", "font-size": "var(--md-label-m-size)" }}>
            {transferList().length}
          </span>
        </div>
        <List>
          <For each={transferList()}>{(t) => <TransferRow t={t} />}</For>
        </List>
      </Surface>
    </Show>
  );
}
