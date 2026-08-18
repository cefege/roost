// Browse-page toolbar: cancel (compact only), back/forward, grid⇄list, the
// show-files toggle, New folder, and the server switcher. Split out of
// BrowsePage.tsx so the page keeps state ownership — every value below arrives
// already computed and every button reports back through a callback.
//
// Callers: BrowsePage.tsx (WorkerBrowsePage).

import { For, Show } from "solid-js";
import { StatusDot } from "./Settings/md/StatusDot.tsx";
import type { Worker } from "@roost/shared/wire";

export function BrowseToolbar(props: {
  compact: boolean;
  viewMode: "grid" | "list";
  showFiles: boolean;
  backEnabled: boolean;
  forwardEnabled: boolean;
  serverFp: string;
  serverLabel: string;
  serverOnline: boolean;
  onlineWorkers: Worker[];
  serverMenuOpen: boolean;
  setServerMenuOpen: (open: boolean) => void;
  onCancel: () => void;
  onBack: () => void;
  onForward: () => void;
  onViewMode: (mode: "grid" | "list") => void;
  onToggleShowFiles: () => void;
  onNewFolder: () => void;
  onSelectServer: (fp: string) => void;
}) {
  return (
    <div class="df-browse-toolbar">
      <Show when={props.compact}>
        <button type="button" class="df-browse-close" data-testid="browse-close"
          aria-label="Cancel" title="Cancel"
          onClick={props.onCancel}>
          <svg width="24" height="24" viewBox="0 0 24 24" fill="none" aria-hidden="true">
            <path d="M18 6L6 18M6 6l12 12" stroke="currentColor" stroke-width="2"
              stroke-linecap="round" stroke-linejoin="round" />
          </svg>
        </button>
      </Show>
      <button type="button" class="df-browse-back" data-testid="browse-back" aria-label="Back"
        onClick={props.onBack} disabled={!props.backEnabled} title="Back"
      >
        <svg width="24" height="24" viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <path d="M15 18l-6-6 6-6" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
      </button>
      <button type="button" class="df-browse-forward" data-testid="browse-forward" aria-label="Forward"
        onClick={props.onForward} disabled={!props.forwardEnabled} title="Forward"
      >
        <svg width="24" height="24" viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <path d="M9 18l6-6-6-6" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
      </button>

      <div class="df-browse-toggle" role="group" aria-label="View mode">
        <button type="button" class="df-browse-toggle-btn" data-testid="browse-view-grid"
          data-active={props.viewMode === "grid" ? "true" : "false"} aria-label="Grid view"
          aria-pressed={props.viewMode === "grid"} onClick={() => props.onViewMode("grid")}
        ><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="3" y="3" width="7" height="7" rx="1" /><rect x="14" y="3" width="7" height="7" rx="1" /><rect x="3" y="14" width="7" height="7" rx="1" /><rect x="14" y="14" width="7" height="7" rx="1" /></svg></button>
        <button type="button" class="df-browse-toggle-btn" data-testid="browse-view-list"
          data-active={props.viewMode === "list" ? "true" : "false"} aria-label="List view"
          aria-pressed={props.viewMode === "list"} onClick={() => props.onViewMode("list")}
        ><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><line x1="8" y1="6" x2="21" y2="6" /><line x1="8" y1="12" x2="21" y2="12" /><line x1="8" y1="18" x2="21" y2="18" /><circle cx="3.5" cy="6" r="1" /><circle cx="3.5" cy="12" r="1" /><circle cx="3.5" cy="18" r="1" /></svg></button>
      </div>
      <div class="df-browse-toolbar-actions">
        <button type="button" class="df-browse-toggle-btn" data-testid="browse-show-files"
          data-active={props.showFiles ? "true" : "false"} aria-pressed={props.showFiles}
          onClick={props.onToggleShowFiles} title="Show files in this folder"
        >
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M14 3v4a1 1 0 0 0 1 1h4" /><path d="M17 21H7a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h7l5 5v11a2 2 0 0 1-2 2z" />
          </svg>
        </button>

        <button type="button" class="df-browse-new" data-testid="browse-new" onClick={props.onNewFolder}>
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
          ><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" /><path d="M12 11v5M9.5 13.5h5" /></svg>
          <span class="df-browse-new-label">New folder</span>
        </button>

        <Show when={props.onlineWorkers.length > 1}>
          <div style={{ position: "relative", "flex-shrink": "0" }}>
            <button type="button" class="df-browse-server" data-testid="browse-server"
              onClick={(e) => { e.stopPropagation(); props.setServerMenuOpen(!props.serverMenuOpen); }} title={props.serverLabel}
            >
              <StatusDot status={props.serverOnline ? "ok" : "idle"} size={7} />
              <span class="df-browse-server-label">{props.serverLabel}</span>
              <span aria-hidden="true" style={{ "font-size": "10px", opacity: "0.8" }}>▼</span>
            </button>
            <Show when={props.serverMenuOpen}>
              <div data-testid="browse-server-menu"
                style={{ position: "absolute", top: "calc(100% + 6px)", right: "0", "min-width": "180px", "z-index": "1", display: "flex", "flex-direction": "column", padding: "4px", background: "var(--md-sys-color-surface-container-high)", border: "1px solid var(--md-sys-color-outline-variant)", "border-radius": "var(--md-shape-md)", "box-shadow": "var(--md-elev-2)" }}
              >
                <For each={props.onlineWorkers}>
                  {(w) => (
                    <button type="button" data-testid="browse-server-option"
                      onClick={(e) => { e.stopPropagation(); props.onSelectServer(String(w.fp)); }}
                      style={{ display: "flex", "align-items": "center", gap: "8px", width: "100%", padding: "6px 10px", "border-radius": "var(--md-shape-sm)", border: "none", background: String(w.fp) === props.serverFp ? "var(--md-sys-color-secondary-container)" : "transparent", color: String(w.fp) === props.serverFp ? "var(--md-sys-color-on-secondary-container)" : "var(--md-sys-color-on-surface)", "font-size": "var(--md-body-s-size)", "font-family": "inherit", cursor: "pointer", "text-align": "left" }}
                    >
                      <StatusDot status="ok" size={7} />
                      <span style={{ flex: "1", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>{w.label}</span>
                      <Show when={String(w.fp) === props.serverFp}><span aria-hidden="true" style={{ opacity: "0.7" }}>✓</span></Show>
                    </button>
                  )}
                </For>
              </div>
            </Show>
          </div>
        </Show>
      </div>
    </div>
  );
}
