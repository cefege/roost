// AuditLogPane — paginated + live table of audit_log rows.
// Subscribes to audit.deltas for real-time inserts; filter bar filters
// client-side. Pagination: "load more" re-queries with cursor.
// M3: filter Card with form-row inputs (mono-spaced for fp/path);
// table Card; status chip color per response class.
// Callers: SettingsRoot.tsx (Audit pane). Depends on: coordClient.auditList,
// registerAuditDelta, md/primitives.

import { createSignal, For, Show, onMount, onCleanup, createMemo } from "solid-js";
import { coordClient } from "../../connect.ts";
import { registerAuditDelta, registerLazySyncDomain } from "../../store/sync.ts";
import { SyncDomain } from "@roost/shared/proto/sync_pb";
import type { AuditRow as PbAuditRow } from "@roost/shared/proto/wire_pb";
import { Card, Button, EmptyState, Icon, TextField } from "./md/primitives.tsx";

interface AuditRow {
  id: number;
  ts: number;
  caller_fp: string | null;
  caller_label: string | null;
  method: string;
  path: string;
  status: number;
  trace_id: string | null;
}

const PAGE_LIMIT = 100;

function statusTone(status: number): { bg: string; fg: string } {
  if (status < 300) return { bg: "var(--md-sys-color-secondary-container)", fg: "var(--md-sys-color-on-secondary-container)" };
  if (status < 400) return { bg: "var(--md-sys-color-surface-container-high)", fg: "var(--md-sys-color-on-surface)" };
  if (status < 500) return { bg: "var(--md-sys-color-tertiary-container)", fg: "var(--md-sys-color-on-tertiary-container)" };
  return { bg: "var(--md-sys-color-error)", fg: "var(--md-on-primary)" };
}

function fmtTs(ts: number): string {
  const d = new Date(ts);
  return d.toISOString().replace("T", " ").slice(0, 19);
}
function shortFp(fp: string | null): string {
  if (!fp) return "—";
  return fp.slice(0, 8) + "…";
}
function rowMatchesFilters(
  row: AuditRow,
  fp: string, method: string, pathSubstr: string,
  statusMin: number, statusMax: number,
): boolean {
  if (fp && !(row.caller_fp ?? "").startsWith(fp)) return false;
  if (method && row.method.toUpperCase() !== method.toUpperCase()) return false;
  if (pathSubstr && !row.path.includes(pathSubstr)) return false;
  if (row.status < statusMin || row.status > statusMax) return false;
  return true;
}
function auditRowsFromProto(rows: readonly PbAuditRow[]): AuditRow[] {
  return rows.map((row) => ({
    id: Number(row.id),
    ts: Number(row.ts),
    caller_fp: row.callerFp ?? null,
    caller_label: row.callerLabel ?? null,
    method: row.method,
    path: row.path,
    status: row.status,
    trace_id: row.traceId ?? null,
  }));
}


export function AuditLogPane() {
  const [allRows, setAllRows] = createSignal<AuditRow[]>([]);
  const [nextCursor, setNextCursor] = createSignal<number | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [loadErr, setLoadErr] = createSignal<string | null>(null);

  const [filterFp, setFilterFp] = createSignal("");
  const [filterMethod, setFilterMethod] = createSignal("");
  const [filterPath, setFilterPath] = createSignal("");
  const [filterStatusMin, setFilterStatusMin] = createSignal("");
  const [filterStatusMax, setFilterStatusMax] = createSignal("");

  const filteredRows = createMemo(() => {
    const fp = filterFp().trim();
    const method = filterMethod().trim();
    const pathSubstr = filterPath().trim();
    const rawMin = parseInt(filterStatusMin().trim(), 10);
    const rawMax = parseInt(filterStatusMax().trim(), 10);
    const statusMin = Number.isFinite(rawMin) ? rawMin : 0;
    const statusMax = Number.isFinite(rawMax) ? rawMax : 999;
    return allRows().filter((r) =>
      rowMatchesFilters(r, fp, method, pathSubstr, statusMin, statusMax),
    );
  });

  async function fetchPage(cursor?: number, replace = false) {
    setLoading(true);
    setLoadErr(null);
    try {
      const result = await coordClient.auditList({
        limit: PAGE_LIMIT,
        cursor: cursor !== undefined ? String(cursor) : undefined,
      });
      const rows = auditRowsFromProto(result.rows);
      if (replace) setAllRows(rows);
      else setAllRows((prev) => [...prev, ...rows]);
      setNextCursor(result.nextCursor ? Number(result.nextCursor) : null);
    } catch (e) {
      setLoadErr(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }

  function clearFilters() {
    setFilterFp(""); setFilterMethod(""); setFilterPath("");
    setFilterStatusMin(""); setFilterStatusMax("");
  }

  onMount(() => {
    // Local consumer first: once the lazy server subscription is emitted, no
    // accepted delta can race past this pane's handler.
    const unsubDeltas = registerAuditDelta((raw) => {
      const row = raw as AuditRow;
      setAllRows((prev) => {
        if (prev.some((existing) => existing.id === row.id)) return prev;
        return [row, ...prev];
      });
    });
    const unsubDomain = registerLazySyncDomain(SyncDomain.AUDIT, async () => {
      setLoading(true);
      setLoadErr(null);
      try {
        const result = await coordClient.auditList({ limit: PAGE_LIMIT });
        const rows = auditRowsFromProto(result.rows);
        const cursor = result.nextCursor ? Number(result.nextCursor) : null;
        return {
          apply: () => {
            setAllRows(rows);
            setNextCursor(cursor);
          },
        };
      } catch (error) {
        setLoadErr(error instanceof Error ? error.message : String(error));
        throw error;
      } finally {
        setLoading(false);
      }
    });
    onCleanup(() => {
      unsubDomain();
      unsubDeltas();
    });
  });

  return (
    <div data-testid="audit-log-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      <Card
        title="Audit log"
        supporting="Read-only log of every authenticated API call. Most recent first. Rotates at 10 000 rows. New rows stream in real time."
      >
        <div style={{
          display: "grid",
          "grid-template-columns": "repeat(auto-fit, minmax(160px, 1fr))",
          gap: "var(--md-space-3)",
        }}>
          <TextField
            testId="audit-filter-fp" label="Caller fp prefix"
            value={filterFp()} onInput={(v) => setFilterFp(v)}
            placeholder="deadbeef"
            style={{ "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace" }}
          />
          <TextField
            testId="audit-filter-method" label="Method"
            value={filterMethod()} onInput={(v) => setFilterMethod(v)}
            placeholder="POST"
          />
          <TextField
            testId="audit-filter-path" label="Path contains"
            value={filterPath()} onInput={(v) => setFilterPath(v)}
            placeholder="Sessions"
            style={{ "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace" }}
          />
          <TextField
            testId="audit-filter-status-min" label="Status ≥"
            value={filterStatusMin()} onInput={(v) => setFilterStatusMin(v)}
            placeholder="200"
          />
          <TextField
            testId="audit-filter-status-max" label="Status ≤"
            value={filterStatusMax()} onInput={(v) => setFilterStatusMax(v)}
            placeholder="299"
          />
        </div>
        <div style={{ display: "flex", "justify-content": "flex-end" }}>
          <Button variant="text" icon="filter_alt_off" onClick={clearFilters} data-testid="audit-filter-clear">
            Clear filters
          </Button>
        </div>
        <Show when={loadErr()}>
          <div data-testid="audit-load-error" style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}>
            <Icon name="error" style={{ color: "var(--md-sys-color-error)" }} />
            <span class="md-body-m" style={{ color: "var(--md-sys-color-error)" }}>{loadErr()}</span>
          </div>
        </Show>
      </Card>

      <Card title="Events">
        <Show
          when={filteredRows().length > 0}
          fallback={
            <EmptyState
              icon="history"
              title={loading() ? "Loading…" : "No matching events"}
              supporting={loading() ? undefined : "Adjust the filters above or wait for new traffic — events stream live."}
            />
          }
        >
          <div style={{ overflow: "auto", "border-radius": "var(--md-shape-sm)", border: "1px solid var(--md-sys-color-outline-variant)" }}>
            <table
              data-testid="audit-log-table"
              style={{ width: "100%", "border-collapse": "collapse" }}
              class="md-body-s"
            >
              <thead>
                <tr style={{ background: "var(--md-sys-color-surface-container-high)" }}>
                  <For each={["Time", "Caller", "Method", "Path", "Status", "Trace"]}>
                    {(h) => (
                      <th
                        class="md-label-s"
                        style={{
                          padding: "10px 12px",
                          "text-align": "left",
                          color: "var(--md-sys-color-on-surface-variant)",
                          "text-transform": "uppercase",
                          "letter-spacing": "0.06em",
                          "border-bottom": "1px solid var(--md-sys-color-outline-variant)",
                          "white-space": "nowrap",
                        }}
                      >
                        {h}
                      </th>
                    )}
                  </For>
                </tr>
              </thead>
              <tbody>
                <For each={filteredRows()}>
                  {(row) => {
                    const tone = statusTone(row.status);
                    return (
                      <tr
                        data-testid="audit-log-row"
                        data-audit-id={row.id}
                        style={{ "border-bottom": "1px solid var(--md-sys-color-outline-variant)" }}
                      >
                        <td style={{ padding: "8px 12px", "white-space": "nowrap" }} title={new Date(row.ts).toISOString()}>
                          {fmtTs(row.ts)}
                        </td>
                        <td style={{ padding: "8px 12px", "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace" }} title={row.caller_fp ?? "unknown"}>
                          <Show when={row.caller_fp} fallback={<span style={{ color: "var(--md-sys-color-on-surface-variant)" }}>—</span>}>
                            <span style={{ color: "var(--md-sys-color-on-surface)" }}>{row.caller_label ?? shortFp(row.caller_fp)}</span>
                            <span style={{ color: "var(--md-sys-color-on-surface-variant)", "margin-left": "6px" }}>
                              {shortFp(row.caller_fp)}
                            </span>
                          </Show>
                        </td>
                        <td style={{
                          padding: "8px 12px", "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace",
                          color: "var(--md-sys-color-primary)", "font-weight": 600,
                        }}>{row.method}</td>
                        <td
                          style={{
                            padding: "8px 12px", "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace",
                            "max-width": "360px", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap",
                          }}
                          title={row.path}
                        >
                          {row.path}
                        </td>
                        <td style={{ padding: "8px 12px" }}>
                          <span class="md-label-m" style={{
                            padding: "2px 10px", "border-radius": "var(--md-shape-full)",
                            background: tone.bg, color: tone.fg, display: "inline-block",
                            "font-variant-numeric": "tabular-nums",
                          }}>
                            {row.status}
                          </span>
                        </td>
                        <td style={{
                          padding: "8px 12px", "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace",
                          color: "var(--md-sys-color-on-surface-variant)", "max-width": "120px",
                        }} title={row.trace_id ?? ""}>
                          {row.trace_id ? row.trace_id.slice(0, 8) + "…" : "—"}
                        </td>
                      </tr>
                    );
                  }}
                </For>
              </tbody>
            </table>
          </div>
        </Show>

        <div style={{ display: "flex", gap: "var(--md-space-3)", "align-items": "center" }}>
          <Show when={nextCursor() !== null}>
            <Button
              variant="tonal"
              icon="expand_more"
              disabled={loading()}
              onClick={() => void fetchPage(nextCursor() ?? undefined)}
              data-testid="audit-load-more"
            >
              {loading() ? "Loading…" : "Load more"}
            </Button>
          </Show>
          <Show when={loading() && nextCursor() === null}>
            <span class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>Loading…</span>
          </Show>
          <span class="md-label-m" style={{ color: "var(--md-sys-color-on-surface-variant)", "margin-left": "auto" }}>
            {filteredRows().length} / {allRows().length} row{allRows().length !== 1 ? "s" : ""}
          </span>
        </div>
      </Card>
    </div>
  );
}
