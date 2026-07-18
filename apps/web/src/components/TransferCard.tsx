// Unified transfer card stack — one M3 surface drives BOTH uploads and
// downloads (name, determinate progress bar, bytes/%, speed, ETA). Replaces the
// upload chip (AttachmentChip) and the download toast so the two flows look and
// behave identically. Portal-mounted bottom-right at App-shell level so it
// survives Terminal pane switches (per feedback_persistent_terminal_deck).
//
// State source: src/store/transfers.ts. "ok"/"dedup" cards auto-dismiss 2s
// after completion; "err" cards persist until dismissed. Card styling mirrors
// PairRequestNotifier's PairCard (same surface/elevation/shape tokens).

import { For, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { transfers, removeTransfer, type Transfer } from "../store/transfers.ts";
import { formatBytes, formatSpeed, formatEta } from "../lib/format.ts";
import "@material/web/progress/linear-progress.js";

export function TransferStack() {
  return (
    <Portal mount={document.body}>
      <div
        data-testid="transfer-stack"
        style={{
          position: "fixed",
          bottom: "20px",
          right: "20px",
          display: "flex",
          "flex-direction": "column",
          gap: "10px",
          "z-index": "9000",
          "pointer-events": "none",
          "max-width": "min(360px, calc(100vw - 40px))",
          "font-family":
            'Roboto, "Helvetica Neue", -apple-system, BlinkMacSystemFont, system-ui, sans-serif',
        }}
      >
        <For each={Object.values(transfers)}>{(t) => <TransferCard t={t} />}</For>
      </div>
    </Portal>
  );
}

function TransferCard(props: { t: Transfer }) {
  const t = () => props.t;
  const done = () => t().state === "ok" || t().state === "dedup" || t().state === "err";
  // % is live during "active"; guard total=0 (unknown/empty) → 0.
  const pct = () => (t().bytes_total > 0 ? Math.round((t().bytes_done / t().bytes_total) * 100) : 0);
  const meta = () => {
    const s = t();
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
    const st = t().state;
    if (st === "err") return "var(--color-err)";
    if (st === "ok" || st === "dedup") return "var(--color-ok)";
    return "var(--text-lo)";
  };
  return (
    <div
      data-testid="transfer-card"
      data-state={t().state}
      data-dir={t().dir}
      style={{
        background: "var(--bg-elev-2)",
        color: "var(--text-hi)",
        "border-radius": "var(--md-shape-md)",
        padding: "12px 14px",
        "box-shadow": "var(--md-elev-3)",
        border: "1px solid var(--border-strong)",
        display: "flex",
        "flex-direction": "column",
        gap: "8px",
        "pointer-events": "auto",
      }}
    >
      <div style={{ display: "flex", "align-items": "center", gap: "8px" }}>
        <span aria-hidden="true" style={{ "font-size": "16px", color: "var(--color-accent)", "flex-shrink": "0" }}>
          {t().dir === "up" ? "↑" : "↓"}
        </span>
        <span
          title={t().name}
          style={{
            flex: "1",
            "min-width": "0",
            "font-size": "var(--md-body-s-size)",
            "font-weight": 500,
            overflow: "hidden",
            "text-overflow": "ellipsis",
            "white-space": "nowrap",
          }}
        >
          {t().name}
        </span>
        <button
          type="button"
          data-testid="transfer-dismiss"
          aria-label="Dismiss"
          onClick={() => removeTransfer(t().id)}
          style={{
            background: "transparent",
            border: "none",
            color: "var(--text-lo)",
            cursor: "pointer",
            "font-size": "16px",
            "line-height": "1",
            padding: "2px",
            "flex-shrink": "0",
          }}
        >✕</button>
      </div>
      <Show when={!done()}>
        <md-linear-progress
          prop:value={t().bytes_total > 0 ? t().bytes_done / t().bytes_total : 0}
          prop:indeterminate={t().state === "hashing" || (t().state === "active" && t().bytes_total === 0)}
        />
      </Show>
      <div style={{ "font-size": "var(--md-body-s-size)", "line-height": "16px", color: metaColor() }}>
        {meta()}
      </div>
    </div>
  );
}
