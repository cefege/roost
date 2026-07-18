// MetricsPane — live telemetry counters from coord.
// Polls coordClient.miscMetrics every 5s. Shows total req/error +
// per-route breakdown sorted by request count descending.
// M3: hero MetricTile row (Uptime / Requests / Errors / Error rate),
// then a Card with the per-route table.
// Callers: SettingsRoot.tsx (Metrics pane). Depends on: coordClient,
// md/primitives.

import { createSignal, onCleanup, onMount, For, Index, Show } from "solid-js";
import { coordClient } from "../../connect.ts";
import { Card, MetricTile, EmptyState, Icon } from "./md/primitives.tsx";
import { isPageVisible } from "../../lib/pageVisible.ts";

interface MetricsSnapshot {
  uptime_ms: number;
  requests: Record<string, number>;
  errors: Record<string, number>;
  total_requests: number;
  total_errors: number;
}

const POLL_INTERVAL_MS = 5_000;

function fmtUptime(ms: number): string {
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (h > 0) return `${h}h ${m}m ${sec}s`;
  if (m > 0) return `${m}m ${sec}s`;
  return `${sec}s`;
}

export function MetricsPane() {
  const [snapshot, setSnapshot] = createSignal<MetricsSnapshot | null>(null);
  const [err, setErr] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);

  async function poll() {
    setLoading(true);
    try {
      const m = await coordClient.miscMetrics({});
      const requests: Record<string, number> = {};
      for (const [k, v] of Object.entries(m.requests)) requests[k] = Number(v);
      const errors: Record<string, number> = {};
      for (const [k, v] of Object.entries(m.errors)) errors[k] = Number(v);
      setSnapshot({
        uptime_ms: Number(m.uptimeMs),
        requests, errors,
        total_requests: Number(m.totalRequests),
        total_errors: Number(m.totalErrors),
      });
      setErr(null);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }

  onMount(() => {
    void poll();
    // Hidden-tab gate: skip the RPC while hidden; next visible tick refreshes.
    const timer = setInterval(() => { if (isPageVisible()) void poll(); }, POLL_INTERVAL_MS);
    onCleanup(() => clearInterval(timer));
  });

  const sortedRoutes = () => {
    const snap = snapshot();
    if (!snap) return [];
    return Object.entries(snap.requests)
      .sort(([, a], [, b]) => b - a)
      .map(([path, count]) => ({
        path, count,
        errors: snap.errors[path] ?? 0,
      }));
  };

  const errorRate = () => {
    const s = snapshot();
    if (!s || s.total_requests === 0) return "—";
    return ((s.total_errors / s.total_requests) * 100).toFixed(1) + "%";
  };

  return (
    <div data-testid="metrics-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      <Card
        title="Coordinator metrics"
        supporting="Live telemetry counters. Refreshes every 5s. Counts reset on coord restart."
      >
        <Show when={err()}>
          <div data-testid="metrics-error" style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}>
            <Icon name="error" style={{ color: "var(--md-sys-color-error)" }} />
            <span class="md-body-m" style={{ color: "var(--md-sys-color-error)" }}>{err()}</span>
          </div>
        </Show>
        <Show when={snapshot()} fallback={
          <Show when={loading()}>
            <span class="md-body-m" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>Loading…</span>
          </Show>
        }>
          {(snap) => (
            <div class="md-metric-grid" data-testid="metrics-summary">
              <MetricTile icon="schedule"      label="Uptime"       value={fmtUptime(snap().uptime_ms)} />
              <MetricTile icon="sync_alt"      label="Requests"     value={snap().total_requests.toLocaleString()} />
              <MetricTile icon="report"        label="Errors"       value={snap().total_errors.toLocaleString()}
                          support={snap().total_errors > 0 ? "4xx + 5xx responses" : "All clear"} />
              <MetricTile icon="trending_down" label="Error rate"   value={errorRate()} />
            </div>
          )}
        </Show>
      </Card>

      <Card title="Per-route activity">
        <Show
          when={sortedRoutes().length > 0}
          fallback={
            <EmptyState
              icon="monitoring"
              title="No requests recorded yet"
              supporting="Counters reset on coord restart. Drive some traffic and metrics will appear here within 5 s."
            />
          }
        >
          <div style={{ overflow: "auto", "border-radius": "var(--md-shape-sm)", border: "1px solid var(--md-sys-color-outline-variant)" }}>
            <table
              data-testid="metrics-table"
              style={{ width: "100%", "border-collapse": "collapse" }}
              class="md-body-s"
            >
              <thead>
                <tr style={{ background: "var(--md-sys-color-surface-container-high)" }}>
                  <For each={[
                    { label: "Route",    align: "left" },
                    { label: "Requests", align: "right" },
                    { label: "Errors",   align: "right" },
                    { label: "Error %",  align: "right" },
                  ]}>
                    {(h) => (
                      <th
                        class="md-label-s"
                        style={{
                          padding: "10px 12px",
                          "text-align": h.align as "left" | "right",
                          color: "var(--md-sys-color-on-surface-variant)",
                          "text-transform": "uppercase",
                          "letter-spacing": "0.06em",
                          "border-bottom": "1px solid var(--md-sys-color-outline-variant)",
                          "white-space": "nowrap",
                        }}
                      >
                        {h.label}
                      </th>
                    )}
                  </For>
                </tr>
              </thead>
              <tbody>
                {/* <Index>: positional identity — each 5s poll mints all-new row
                    objects, so a keyed <For> tore down and recreated every <tr>;
                    Index keeps the DOM and updates only changed text/attrs. */}
                <Index each={sortedRoutes()}>
                  {(row) => (
                    <tr
                      data-testid="metrics-row"
                      data-route={row().path}
                      style={{ "border-bottom": "1px solid var(--md-sys-color-outline-variant)" }}
                    >
                      <td
                        style={{ padding: "8px 12px", "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace", color: "var(--md-sys-color-primary)", "max-width": "440px", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}
                        title={row().path}
                      >
                        {row().path}
                      </td>
                      <td style={{ padding: "8px 12px", "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace", "text-align": "right", "font-variant-numeric": "tabular-nums" }}>
                        {row().count.toLocaleString()}
                      </td>
                      <td style={{
                        padding: "8px 12px", "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace", "text-align": "right",
                        color: row().errors > 0 ? "var(--md-sys-color-error)" : "var(--md-sys-color-on-surface-variant)",
                        "font-variant-numeric": "tabular-nums",
                      }}>
                        {row().errors.toLocaleString()}
                      </td>
                      <td style={{
                        padding: "8px 12px", "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace", "text-align": "right",
                        color: row().errors > 0 ? "var(--md-sys-color-tertiary)" : "var(--md-sys-color-on-surface-variant)",
                        "font-variant-numeric": "tabular-nums",
                      }}>
                        {row().count > 0 ? ((row().errors / row().count) * 100).toFixed(1) + "%" : "—"}
                      </td>
                    </tr>
                  )}
                </Index>
              </tbody>
            </table>
          </div>
        </Show>
      </Card>
    </div>
  );
}
