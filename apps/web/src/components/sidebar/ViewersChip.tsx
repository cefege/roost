// Per-session viewer chip stack — one Material-3 person pill per browser
// fingerprint currently viewing the session. Source:
// rootStore.session_viewers[sid] (coord pushes via globalPresenceBus
// kind="viewers"; folded by store/sync.ts). Each entry carries
// { fp, cols, rows, lastMs?, label? } — label comes from
// authorized_keys.label and falls back to the 8-char fp prefix.
// Color is a deterministic fp-hash hue so the same browser shows the
// same tone across every SessionRow it's looking at; tabs within one
// browser share the hue (composite viewer key `${fp}:${tab_id}` —
// hash on the fp prefix only).

import { For, Show, createMemo } from "solid-js";
import { rootStore } from "../../store/root.ts";
import { colorForFp } from "../../lib/fpColor.ts";

interface ViewersChipProps { sessionId: string }

// SCD policy sizes the PTY to min(cols) × min(rows) across viewers, so the
// "controlling" viewer(s) are whichever ones hold the binding minimum —
// width and height can be clamped by DIFFERENT viewers. Their avatar
// glows so it's obvious at a glance which small window is constraining
// everyone. See [[feedback_viewport_scd_min_policy]].
interface ViewerEntry { fp: string; cols: number; rows: number }
function bindingMins(entries: ViewerEntry[]): { cols: number; rows: number } {
  let cols = Infinity, rows = Infinity;
  for (const e of entries) {
    if (e.cols > 0 && e.cols < cols) cols = e.cols;
    if (e.rows > 0 && e.rows < rows) rows = e.rows;
  }
  return { cols, rows };
}

function displayName(entry: { fp: string; label?: string }): string {
  if (entry.label && entry.label.trim().length > 0) return entry.label;
  const colon = entry.fp.indexOf(":");
  const base = colon >= 0 ? entry.fp.slice(0, colon) : entry.fp;
  return base.slice(0, 8);
}

export function ViewersChip(props: ViewersChipProps) {
  const entries = () => rootStore.session_viewers[props.sessionId] ?? [];
  const mins = createMemo(() => bindingMins(entries() as ViewerEntry[]));
  // Only flag a controller when there's contention (≥2 viewers) — a sole
  // viewer trivially "controls" but there's nothing to disambiguate.
  const isController = (e: ViewerEntry): boolean =>
    entries().length > 1 && e.cols > 0 && e.rows > 0 &&
    (e.cols === mins().cols || e.rows === mins().rows);
  return (
    <Show when={entries().length > 0}>
      <span
        data-testid={`session-viewers-${props.sessionId}`}
        data-viewer-count={entries().length}
        style={{
          display: "inline-flex",
          "align-items": "center",
          gap: "4px",
          "margin-right": "6px",
          "font-family":
            'Roboto, "Helvetica Neue", -apple-system, BlinkMacSystemFont, system-ui, sans-serif',
        }}
      >
        <For each={entries()}>
          {(e, idx) => {
            const c = colorForFp(e.fp);
            const name = displayName(e);
            const controlling = isController(e as ViewerEntry);
            // Face-only avatar; identity revealed on hover via native title.
            // Stacked with a slight overlap (negative margin past the first)
            // so N viewers stay compact instead of eating the row width.
            // The size-controlling viewer (binding SCD min) gets an accent
            // glow ring + raised z-index so it reads above its neighbors.
            return (
              <span
                data-viewer-fp={e.fp}
                data-viewer-label={e.label ?? ""}
                data-viewer-name={name}
                data-controlling={controlling ? "true" : "false"}
                title={(e.label ? `${e.label} (${e.fp.slice(0, 8)})` : `viewer ${e.fp.slice(0, 8)}`) + (controlling ? " — controls terminal size" : "")}
                style={{
                  display: "inline-flex",
                  "align-items": "center",
                  "justify-content": "center",
                  width: "22px",
                  height: "22px",
                  "margin-left": idx() === 0 ? "0" : "-7px",
                  background: c.bg,
                  color: c.fg,
                  "border-radius": "50%",
                  // M3 avatar: monogram on a tonal circle, separated from its
                  // neighbours by a surface-colored ring; the SCD-controlling
                  // viewer gets a primary halo.
                  "box-shadow": controlling
                    ? "0 0 0 2px var(--md-sys-color-surface), 0 0 0 3px var(--md-sys-color-primary), 0 0 8px 1px color-mix(in srgb, var(--md-sys-color-primary) 60%, transparent)"
                    : "0 0 0 2px var(--md-sys-color-surface)",
                  "z-index": controlling ? "1" : "0",
                  position: "relative",
                  "flex-shrink": "0",
                  cursor: "default",
                  "font-size": "11px",
                  "font-weight": "600",
                  "line-height": "1",
                  "letter-spacing": "0.2px",
                  "user-select": "none",
                }}
              >
                {name.charAt(0).toUpperCase()}
              </span>
            );
          }}
        </For>
      </span>
    </Show>
  );
}
