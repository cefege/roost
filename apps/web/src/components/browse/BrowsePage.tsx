// Keys the file browser by worker route parameter so worker-local history and
// asynchronous listing ownership reset together on server changes.
// The worker page owns browsing state; this module preserves the route-facing
// BrowsePage and BrowseRedirect exports consumed by App.tsx.

import { Show } from "solid-js";
import { useParams } from "@solidjs/router";
import { WorkerBrowsePage } from "./WorkerBrowsePage.tsx";

export { BrowseRedirect } from "./BrowseRedirect.tsx";

export function BrowsePage() {
  const params = useParams<{ workerFp: string }>();
  return (
    <Show when={params.workerFp} keyed>
      {(workerFp) => <WorkerBrowsePage workerFp={workerFp} />}
    </Show>
  );
}
