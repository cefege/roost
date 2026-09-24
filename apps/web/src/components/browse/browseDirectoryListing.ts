// Directory listing for the folder picker: one auth-fenced FilesListDir per
// (machine, path, retry), the loading and error state the content region
// paints, and the diag trail for a listing and its failures.
// WorkerBrowsePage owns the path signals and renders the result.
//
// Callers: WorkerBrowsePage.tsx.

import { createEffect, createSignal, onCleanup, type Accessor } from "solid-js";
import { diag } from "@roost/observability/diag";
import { coordClient } from "../../client/rpc/connect.ts";
import {
  captureAuthResourceToken,
  isCurrentAuthResourceToken,
} from "../../store/auth-boundary.ts";
import { browseErrorMessage } from "../../lib/browseErrorMessage.ts";
import type { BrowseEntry } from "../../lib/browseEntries.ts";
import type { WorkerFp } from "@roost/protocol/wire";

export interface BrowseDirectoryListing {
  entries: Accessor<BrowseEntry[]>;
  /** Path the worker resolved ("~" expanded), or null before the first reply. */
  resolvedPath: Accessor<string | null>;
  loading: Accessor<boolean>;
  /** Reader-facing failure copy; null while the last listing stands. */
  error: Accessor<string | null>;
  reload: () => void;
}

export function createBrowseDirectoryListing(deps: {
  workerFp: Accessor<string>;
  path: Accessor<string>;
  scoped: Accessor<boolean>;
}): BrowseDirectoryListing {
  const [data, setData] = createSignal<{ resolved: string; entries: BrowseEntry[] } | null>(null);
  // Starts loading: this effect runs after the first render pass, and a false
  // here paints "Empty folder" for that frame.
  const [loading, setLoading] = createSignal(true);
  const [error, setError] = createSignal<string | null>(null);
  const [reloadNonce, setReloadNonce] = createSignal(0);

  createEffect(() => {
    const fp = deps.workerFp();
    const dir = deps.path();
    reloadNonce();
    if (!fp || !deps.scoped()) {
      setData(null);
      setLoading(false);
      return;
    }
    const authToken = captureAuthResourceToken();
    let cancelled = false;
    setLoading(true);
    coordClient.filesListDir({ workerFp: fp as unknown as WorkerFp, path: dir })
      .then((response) => {
        if (cancelled || !isCurrentAuthResourceToken(authToken)) return;
        const entries = response.entries.map((entry) => ({
          name: entry.name, isDir: entry.isDir, mtimeMs: Number(entry.mtimeMs),
        }));
        const resolved = response.resolvedPath || dir;
        setData({ resolved, entries });
        setError(null);
        diag("browse.listed", {
          worker_fp: fp,
          path: dir,
          resolved,
          folders: entries.filter((entry) => entry.isDir).length,
          files: entries.filter((entry) => !entry.isDir).length,
        });
      })
      .catch((failure: unknown) => {
        if (cancelled || !isCurrentAuthResourceToken(authToken)) return;
        setData(null);
        setError(browseErrorMessage(failure));
        // The mapped copy is what the user reads; the machine's own words stay
        // in the diag line so the failure remains greppable.
        diag("browse.list_failed", {
          worker_fp: fp,
          path: dir,
          error: failure instanceof Error ? failure.message : String(failure),
        });
      })
      .finally(() => {
        if (cancelled || !isCurrentAuthResourceToken(authToken)) return;
        setLoading(false);
      });
    onCleanup(() => { cancelled = true; });
  });

  return {
    entries: () => data()?.entries ?? [],
    resolvedPath: () => data()?.resolved ?? null,
    loading,
    error,
    reload: () => setReloadNonce((nonce) => nonce + 1),
  };
}
