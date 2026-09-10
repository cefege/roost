// Desktop status bar. Projects only coordinator, worker, session, and agent state already held by Roost.
// AppShell mounts this fixed footer; no synthetic source-control or remote status is invented.
// Coordinator reachability follows the same health snapshot and stale-window semantics as ConnectionBanner.

import { createMemo, createSignal, onCleanup, onMount, Show } from "solid-js";
import type { CoordHealthSnapshot } from "../ConnectionBanner.tsx";
import { StatusDot } from "../Settings/md/primitives.tsx";
import { rootStore } from "../../store/root.ts";
import { activeSessionForPath } from "../../store/selectors.ts";
import { workerOnline } from "../../store/sync-routable.ts";
import { seenAgentRevision } from "../../lib/agentSeen.ts";
import { AGENT_STATUS_PRESENTATION, deriveAgentStatusLevel } from "../../lib/agentStatus.ts";
import { workbenchTitle } from "../../lib/workbenchTitle.ts";
import { isPageVisible } from "../../lib/pageVisible.ts";
import { useLocation } from "@solidjs/router";

type StatusDotKind = "ok" | "idle" | "offline" | "warn" | "info";

type CoordinatorState = {
  label: string;
  status: StatusDotKind;
};

const COORD_STALE_MS = 10_000;

function coordinatorHealth(): CoordHealthSnapshot | null {
  if (typeof window === "undefined") return null;
  return (window as Window & { __roostCoordHealth?: CoordHealthSnapshot }).__roostCoordHealth ?? null;
}

function coordinatorState(identityKnown: boolean): CoordinatorState {
  if (typeof navigator !== "undefined" && !navigator.onLine) {
    return { label: "Offline", status: "offline" };
  }

  const health = coordinatorHealth();
  const lastSuccessMs = health?.lastSuccessMs;
  const stale = lastSuccessMs !== null
    && lastSuccessMs !== undefined
    && isPageVisible()
    && performance.now() - lastSuccessMs > COORD_STALE_MS;
  if (stale || health?.lastResult?.kind === "unreachable") {
    return { label: "Coordinator unreachable", status: "offline" };
  }
  if (!identityKnown || lastSuccessMs === null || lastSuccessMs === undefined) {
    return { label: "Syncing", status: "idle" };
  }
  return { label: "Synced", status: "ok" };
}



export function WorkbenchStatusBar() {
  const location = useLocation();
  const [healthTick, setHealthTick] = createSignal(0);
  const activeSession = createMemo(() => activeSessionForPath(location.pathname));
  const syncState = createMemo(() => {
    healthTick();
    return coordinatorState(rootStore.coord_identity !== null);
  });
  const activeWorker = createMemo(() => {
    const session = activeSession();
    return session ? rootStore.workers[session.worker_fp] : undefined;
  });
  const activeAgent = createMemo(() => {
    const session = activeSession();
    if (!session) return null;
    const status = rootStore.agent_status[session.id];
    if (!status) return null;
    const level = deriveAgentStatusLevel(status, seenAgentRevision(status));
    if (level === "unknown") return null;
    const presentation = AGENT_STATUS_PRESENTATION[level];
    return { label: presentation.label, status: presentation.dotStatus };
  });
  const sessionContext = createMemo(() => {
    const session = activeSession();
    if (!session) return null;
    const title = workbenchTitle(location.pathname);
    const folder = session.spawn_cwd ?? session.cwd;
    return folder && folder !== title ? `${title} · ${folder}` : title;
  });
  const openSessionCount = createMemo(() =>
    Object.values(rootStore.sessions).filter((session) => session.status === "open").length,
  );
  const workerCounts = createMemo(() => {
    const workers = Object.values(rootStore.workers);
    return { online: workers.filter(workerOnline).length, total: workers.length };
  });
  const revision = createMemo(() => rootStore.coord_identity?.git_sha.slice(0, 7) ?? null);

  onMount(() => {
    const refreshHealth = () => setHealthTick((value) => value + 1);
    const timer = window.setInterval(refreshHealth, 2_000);
    window.addEventListener("online", refreshHealth);
    window.addEventListener("offline", refreshHealth);
    onCleanup(() => {
      window.clearInterval(timer);
      window.removeEventListener("online", refreshHealth);
      window.removeEventListener("offline", refreshHealth);
    });
  });

  return (
    <footer class="workbench-status-bar" data-testid="workbench-status-bar" aria-label="Workbench status">
      <div class="workbench-status-bar__left">
        <span class="workbench-status-item" data-testid="workbench-status-sync" data-status={syncState().status}>
          <StatusDot status={syncState().status} />
          <span>{syncState().label}</span>
        </span>
        <Show when={activeWorker()}>
          {(worker) => (
            <span class="workbench-status-item workbench-status-item--optional" data-testid="workbench-status-worker">
              <StatusDot status={workerOnline(worker()) ? "ok" : "offline"} />
              <span>{worker().label || String(worker().fp)}</span>
            </span>
          )}
        </Show>
        <Show when={activeAgent()}>
          {(agent) => (
            <span class="workbench-status-item workbench-status-item--optional" data-testid="workbench-status-agent">
              <StatusDot status={agent().status} />
              <span>{agent().label}</span>
            </span>
          )}
        </Show>
        <Show when={sessionContext()}>
          {(context) => (
            <span class="workbench-status-item workbench-status-item--context" data-testid="workbench-status-context">
              {context()}
            </span>
          )}
        </Show>
      </div>
      <div class="workbench-status-bar__right">
        <span class="workbench-status-item" data-testid="workbench-status-counts">
          {openSessionCount()} {openSessionCount() === 1 ? "session" : "sessions"}
          <span aria-hidden="true">·</span>
          {workerCounts().online}/{workerCounts().total} workers
        </span>
        <Show when={revision()}>
          {(shortRevision) => (
            <span class="workbench-status-item workbench-status-item--revision" data-testid="workbench-status-revision">
              {shortRevision()}
            </span>
          )}
        </Show>
      </div>
    </footer>
  );
}
