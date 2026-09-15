// Transfer row — renders one upload or download within TransferStack.
// TransferStack owns the sole popup surface and supplies a reactive job record.
// State transitions and dismissal remain in src/store/transfers.ts.

import { Show } from "solid-js";
import { IconButton, ListRow } from "./Settings/md/primitives.tsx";
import { formatBytes, formatSpeed, formatEta } from "../lib/format.ts";
import { removeTransfer, type Transfer } from "../store/transfers.ts";

export function TransferRow(props: { t: Transfer }) {
  const t = () => props.t;
  const done = () => t().state === "ok" || t().state === "dedup" || t().state === "err";
  const pct = () => (t().bytes_total > 0 ? Math.round((t().bytes_done / t().bytes_total) * 100) : 0);
  const progressValue = () => {
    const transfer = t();
    if (
      transfer.state === "queued"
      || transfer.state === "hashing"
      || (transfer.state === "active" && transfer.bytes_total === 0)
    ) {
      return undefined;
    }
    if (transfer.bytes_total === 0) return 0;
    return Math.min(1, Math.max(0, transfer.bytes_done / transfer.bytes_total));
  };
  const meta = () => {
    const s = t();
    if (s.state === "queued") return "Queued…";
    if (s.state === "hashing") return "Checking…";
    if (s.state === "dedup") return "Already uploaded · reused";
    if (s.state === "err") return s.err ?? "Failed";
    if (s.state === "ok") return `${s.dir === "up" ? "Uploaded" : "Downloaded"} · ${formatBytes(s.bytes_total)}`;
    const size = s.bytes_total > 0 ? ` / ${formatBytes(s.bytes_total)}` : "";
    const base = `${formatBytes(s.bytes_done)}${size} · ${pct()}% · ${formatSpeed(s.speed)}`;
    const eta = formatEta(s.eta_s);
    return eta ? `${base} · ${eta} left` : base;
  };
  const metaColor = () => {
    const state = t().state;
    if (state === "err") return "var(--md-sys-color-error)";
    if (state === "ok" || state === "dedup") return "var(--md-sys-color-tertiary)";
    return "var(--md-sys-color-on-surface-variant)";
  };
  return (
    <ListRow
      testId="transfer-row"
      leading={t().dir === "up" ? "upload" : "download"}
      headline={<span title={t().name}>{t().name}</span>}
      support={
        <span style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-1)" }}>
          <Show
            when={!done() && progressValue() !== undefined}
            fallback={
              <Show when={!done()}>
                <progress aria-label="Transfer progress" max={1} style={{ width: "100%", height: "var(--md-space-1)", "accent-color": "var(--md-sys-color-primary)" }} />
              </Show>
            }
          >
            <progress aria-label="Transfer progress" max={1} value={progressValue()!} style={{ width: "100%", height: "var(--md-space-1)", "accent-color": "var(--md-sys-color-primary)" }} />
          </Show>
          <span style={{ "font-size": "var(--md-body-s-size)", "line-height": "var(--md-body-s-line)", color: metaColor() }}>
            {meta()}
          </span>
        </span>
      }
      trailing={
        <IconButton
          size="icon-xs"
          data-testid="transfer-dismiss"
          icon="close"
          label={`Dismiss ${t().name}`}
          onClick={() => removeTransfer(t().id)}
        />
      }
    />
  );
}
