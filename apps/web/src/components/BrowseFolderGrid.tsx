// Browse-page results area: loading/empty states plus the folder+file grid and
// its dense-list alternative. Split out of BrowsePage.tsx; the page owns the
// listing, the filters, the activity memos, and the keyboard cursor — this file
// paints them and reports clicks/hovers back.
//
// Callers: BrowsePage.tsx (WorkerBrowsePage).

import { For, Show } from "solid-js";
import { childPath } from "../lib/folderPalette.ts";
import { colorForFp } from "../lib/fpColor.ts";
import type { FolderActivity } from "../lib/folderActivity.ts";
import { FolderGlyph } from "./FolderGlyph.tsx";
import { FileGlyph } from "./FileGlyph.tsx";

export interface DirEntry { name: string; isDir: boolean; mtimeMs: number }

function relativeTime(ms: number): string {
  if (!ms) return "";
  const diff = Date.now() - ms;
  if (diff < 60_000) return "just now";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
  if (diff < 604_800_000) return `${Math.floor(diff / 86_400_000)}d ago`;
  return new Date(ms).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}
// Self-explanatory timestamp: clock glyph + relative text, with the full
// locale date+time in the `title` so a hover says exactly what "5m ago" means.
function MetaTime(props: { ms: number; class: string }) {
  return (
    <Show when={props.ms > 0}>
      <span class={props.class} title={`Modified ${new Date(props.ms).toLocaleString()}`}>
        <svg class="df-browse-meta-clock" width="11" height="11" viewBox="0 0 24 24" fill="none"
          stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" />
        </svg>
        {relativeTime(props.ms)}
      </span>
    </Show>
  );
}

export function BrowseFolderGrid(props: {
  loading: boolean;
  /** Already filtered + sorted by the page. */
  dirs: DirEntry[];
  files: DirEntry[];
  serverFp: string;
  serverOnline: boolean;
  /** Resolved current directory (cwdNow) — the base for each child path. */
  cwd: string;
  viewMode: "grid" | "list";
  showFiles: boolean;
  activeIdx: number;
  activity: Map<string, FolderActivity>;
  subtitles: Map<string, string>;
  onActivate: (idx: number) => void;
  onDrill: (name: string) => void;
  onNewFolder: () => void;
  setAreaRef: (el: HTMLDivElement) => void;
}) {
  return (
    <div ref={props.setAreaRef} class="df-browse-area" tabIndex="-1">
      <Show when={props.loading && props.dirs.length === 0}>
        <div class="df-browse-empty">Loading…</div>
      </Show>
      <Show when={!props.loading && props.dirs.length === 0}>
        <div class="df-browse-empty">
          <div class="df-browse-empty-icon"><FolderGlyph size={24} /></div>
          {props.serverOnline ? "Empty folder" : "Server offline"}
          <Show when={props.serverOnline} fallback={
            <span class="df-browse-empty-sub">Reconnect to this server to browse folders</span>
          }>
            <span class="df-browse-empty-sub">Create a new folder or open a terminal here</span>
            <div class="df-browse-empty-actions">
              <button type="button" class="df-browse-new" onClick={props.onNewFolder}>
                <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                  stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
                ><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" /><path d="M12 11v5M9.5 13.5h5" /></svg>
                New folder
              </button>
            </div>
          </Show>
        </div>
      </Show>

      <Show when={props.viewMode === "grid"} fallback={
        <div class="df-browse-list">
          <For each={props.dirs}>
            {(d, i) => {
              const path = childPath(props.serverFp, props.cwd, d.name);
              const activity = props.activity.get(path);
              const terminals = activity?.terminals ?? 0;
              return (
                <button type="button" class="df-browse-row" data-testid="browse-row"
                  data-active={props.activeIdx === i() ? "true" : "false"}
                  onClick={() => props.onDrill(d.name)} onmouseenter={() => props.onActivate(i())}
                >
                  <span class="df-browse-row-icon">
                    <FolderGlyph size={20} />
                  </span>
                  <span class="df-browse-row-name">{d.name}</span>
                  <MetaTime ms={d.mtimeMs} class="df-browse-row-meta" />
                  <Show when={terminals > 0}>
                    <span class="df-browse-row-badges">
                      <span class="df-browse-badge df-browse-badge-terminals">{terminals}</span>
                    </span>
                  </Show>
                  <span class="df-browse-row-chev" aria-hidden="true">›</span>
                </button>
              );
            }}
          </For>
          <Show when={props.showFiles}>
            <For each={props.files}>
              {(f) => (
                <div class="df-browse-row df-browse-row-file" data-testid="browse-file-row" aria-label={f.name}>
                  <span class="df-browse-row-icon"><FileGlyph size={20} /></span>
                  <span class="df-browse-row-name">{f.name}</span>
                  <MetaTime ms={f.mtimeMs} class="df-browse-row-meta" />
                </div>
              )}
            </For>
          </Show>
        </div>
      }>
        <div class="df-browse-grid">
          <For each={props.dirs}>
            {(d, i) => {
              const path = childPath(props.serverFp, props.cwd, d.name);
              const activity = props.activity.get(path);
              const terminals = activity?.terminals ?? 0;
              const subtitle = props.subtitles.get(path);
              const hue = colorForFp(props.serverFp).hue;
              return (
                <button type="button" class="df-browse-tile" data-testid="browse-tile"
                  data-active={props.activeIdx === i() ? "true" : "false"}
                  onClick={() => props.onDrill(d.name)} onmouseenter={() => props.onActivate(i())}
                >
                  <span class="df-browse-tile-icon" style={{ color: `hsl(${hue} 48% 42%)` }}>
                    <FolderGlyph size={22} />
                  </span>
                  <span class="df-browse-tile-text">
                    <span class="df-browse-tile-name">{d.name}</span>
                    <Show when={subtitle} fallback={<MetaTime ms={d.mtimeMs} class="df-browse-tile-meta" />}>
                      <span class="df-browse-tile-subtitle">{subtitle}</span>
                    </Show>
                  </span>
                  <Show when={terminals > 0}>
                    <span class="df-browse-tile-badges">
                      <span class="df-browse-badge df-browse-badge-terminals">{terminals}</span>
                    </span>
                  </Show>
                </button>
              );
            }}
          </For>
          <Show when={props.showFiles}>
            <For each={props.files}>
              {(f) => (
                <div class="df-browse-tile df-browse-tile-file" data-testid="browse-file-tile" aria-label={f.name}>
                  <span class="df-browse-tile-icon" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>
                    <FileGlyph size={22} />
                  </span>
                  <span class="df-browse-tile-text">
                    <span class="df-browse-tile-name">{f.name}</span>
                    <MetaTime ms={f.mtimeMs} class="df-browse-tile-meta" />
                  </span>
                </div>
              )}
            </For>
          </Show>
        </div>
      </Show>
    </div>
  );
}
