// Folder-picker content region: a dense responsive grid of folder and file
// entries plus every state that region can show — loading skeletons, a named
// listing failure, an offline machine, an empty folder, and a filter that
// matched nothing. WorkerBrowsePage owns the listing, the filter, the
// terminal-count subtitles, and the keyboard cursor; this file paints them.
//
// Callers: WorkerBrowsePage.tsx.

import { For, Show, Switch, Match, type JSX } from "solid-js";
import { childPath } from "../../lib/folderPalette.ts";
import { relativeEntryTime, type BrowseEntry } from "../../lib/browseEntries.ts";
import { workerFileHref } from "../../lib/nativePath.ts";
import { Button } from "../Settings/md/Button.tsx";
import { Chip } from "../Settings/md/Chip.tsx";
import { EmptyState } from "../Settings/md/EmptyState.tsx";
import { List } from "../Settings/md/List.tsx";
import { ListRow } from "../Settings/md/ListRow.tsx";
import { Skeleton } from "../Settings/md/Skeleton.tsx";

export type BrowseListingStatus = "loading" | "ready" | "error" | "offline";

const SKELETON_ROWS = [0, 1, 2, 3, 4, 5, 6, 7];

export function BrowseEntryList(props: {
  status: BrowseListingStatus;
  loadingCaption: string;
  folders: BrowseEntry[];
  files: BrowseEntry[];
  showFiles: boolean;
  /** Active in-folder filter; drives the no-matches state's copy. */
  filter: string;
  serverFp: string;
  /** Resolved current directory (cwdNow) — the base for each child path. */
  cwd: string;
  activeIdx: number;
  /** Per-child-path count of terminals open in that folder. */
  terminalCounts: Map<string, number>;
  errorMessage: string | null;
  /** Content that scrolls WITH the entries (the recents strip), so the picker
   *  keeps no permanent chrome above the grid. */
  header?: JSX.Element;
  onDrill: (name: string) => void;
  onClearFilter: () => void;
  onRetry: () => void;
  setAreaRef: (element: HTMLDivElement) => void;
}): JSX.Element {
  return (
    <div ref={props.setAreaRef} class="df-browse-area" tabIndex="-1">
      <Show when={props.header}>{props.header}</Show>
      <Switch>
        <Match when={props.status === "loading"}>
          <div data-testid="browse-loading" role="status" aria-live="polite" aria-busy="true">
            <div aria-hidden="true">
              <List layout="grid">
                <For each={SKELETON_ROWS}>
                  {() => <ListRow dense headline={<Skeleton />} />}
                </For>
              </List>
            </div>
            <div class="df-browse-loading-caption md-label-s">{props.loadingCaption}</div>
          </div>
        </Match>

        <Match when={props.status === "error"}>
          <div data-testid="browse-listing-error" role="status" aria-live="polite">
            <EmptyState
              icon="cloud_off"
              title="Couldn't read this folder"
              supporting={props.errorMessage ?? ""}
              action={
                <Button variant="secondary" data-testid="browse-retry" onClick={props.onRetry}>
                  Retry
                </Button>
              }
            />
          </div>
        </Match>

        <Match when={props.status === "offline"}>
          <div data-testid="browse-offline">
            <EmptyState
              icon="cloud_off"
              title="Machine offline"
              supporting="Reconnect to this machine to browse its folders."
            />
          </div>
        </Match>

        <Match when={props.folders.length === 0 && (!props.showFiles || props.files.length === 0)}>
          <Show
            when={props.filter.trim() !== ""}
            fallback={
              <div data-testid="browse-empty">
                <EmptyState
                  icon="folder_open"
                  title="Empty folder"
                  supporting="No subfolders here. Create one, or open a terminal in this folder."
                />
              </div>
            }
          >
            <div data-testid="browse-no-matches">
              <EmptyState
                icon="search_off"
                title="No matches"
                supporting={`Nothing in this folder matches “${props.filter.trim()}”.`}
                action={
                  <Button variant="secondary" data-testid="browse-clear-filter"
                    onClick={props.onClearFilter}>
                    Clear filter
                  </Button>
                }
              />
            </div>
          </Show>
        </Match>

        <Match when={props.status === "ready"}>
          <List layout="grid">
            <For each={props.folders}>
              {(entry, index) => {
                const terminals = props.terminalCounts.get(childPath(props.serverFp, props.cwd, entry.name));
                return (
                  <ListRow
                    dense
                    leading="folder"
                    headline={<span title={entry.name}>{entry.name}</span>}
                    support={relativeEntryTime(entry.mtimeMs)}
                    trailing={
                      <Show when={terminals}>
                        {(count) => (
                          <Chip label={String(count())} icon="terminal"
                            title={`${count()} terminal${count() === 1 ? "" : "s"}`} />
                        )}
                      </Show>
                    }
                    onClick={() => props.onDrill(entry.name)}
                    selected={props.activeIdx === index()}
                    testId="browse-row"
                  />
                );
              }}
            </For>
            <Show when={props.showFiles}>
              <For each={props.files}>
                {(entry) => (
                  <ListRow
                    dense
                    leading="description"
                    headline={<span title={entry.name}>{entry.name}</span>}
                    support={relativeEntryTime(entry.mtimeMs)}
                    href={workerFileHref(props.serverFp, childPath(props.serverFp, props.cwd, entry.name))}
                    testId="browse-file-row"
                  />
                )}
              </For>
            </Show>
          </List>
        </Match>
      </Switch>
    </div>
  );
}
