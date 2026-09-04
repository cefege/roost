// Chooses a worker for the server-less browse route before the file UI mounts.
// Recent online session activity wins, then any online worker, with Home as the
// safe fallback. App.tsx reaches this through BrowsePage's preserved re-export.

import { createMemo } from "solid-js";
import { Navigate } from "@solidjs/router";
import { rootStore } from "../store/root.ts";
import { allSessions } from "../store/selectors.ts";
import { workerOnline } from "../store/sync.ts";
import { browseHref } from "../routes.ts";
// /browse (no server) → most-recent online server, else Home.
export function BrowseRedirect() {
  const fp = createMemo(() => {
    const recent = [...allSessions()].sort((a, b) => b.created_at - a.created_at).find((s) => { const w = rootStore.workers[s.worker_fp]; return w ? workerOnline(w) : false; });
    return recent?.worker_fp ?? Object.values(rootStore.workers).find(workerOnline)?.fp;
  });
  const resolved = fp();
  return <Navigate href={resolved ? browseHref(resolved) : "/"} />;
}
