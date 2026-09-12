// Browse-page results area: loading/empty states plus the folder+file grid and
// its dense-list alternative. Split out of BrowsePage.tsx; the page owns the
// listing, the filters, the activity memos, and the keyboard cursor — this file
// paints them and reports clicks/hovers back.
//
// Callers: BrowsePage.tsx (WorkerBrowsePage).

import { For, Show } from "solid-js";
import { childPath } from "../lib/folderPalette.ts";
import type { FolderActivity } from "../lib/folderActivity.ts";
import { FolderGlyph } from "./FolderGlyph.tsx";
import { FileGlyph } from "./FileGlyph.tsx";
import { Icon } from "./Settings/md/Icon.tsx";

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
        <Icon name="schedule" class="df-browse-meta-clock" />
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
  setAreaRef: (el: HTMLDivElement) => void;
}) {
  return (
    <div ref={props.setAreaRef} class="df-browse-area" tabIndex="-1">
      <Show when={props.loading && props.dirs.length === 0}>
        <div class="df-browse-empty">Loading…</div>
      </Show>
      <Show when={!props.loading && props.dirs.length === 0}>
        <div class="df-browse-empty">
          <div class="df-browse-empty-icon"><FolderGlyph /></div>
          {props.serverOnline ? "Empty folder" : "Server offline"}
          <Show when={props.serverOnline} fallback={
            <span class="df-browse-empty-sub">Reconnect to this server to browse folders</span>
          }>
            <span class="df-browse-empty-sub">Use New folder in the toolbar or open a terminal here</span>
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
                    <FolderGlyph />
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
                  <span class="df-browse-row-icon"><FileGlyph /></span>
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
              return (
                <button type="button" class="df-browse-tile" data-testid="browse-tile"
                  data-active={props.activeIdx === i() ? "true" : "false"}
                  onClick={() => props.onDrill(d.name)} onmouseenter={() => props.onActivate(i())}
                >
                  <span class="df-browse-tile-icon">
                    <FolderGlyph />
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
                  <span class="df-browse-tile-icon">
                    <FileGlyph />
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
