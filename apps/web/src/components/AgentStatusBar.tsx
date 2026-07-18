import { isPageVisible } from "../lib/pageVisible.ts";
// M3 agent status bar — glass-blurred pill pinned at the bottom of
// MainPane for the currently-active claude session. Renders ONLY when
// the active session is kind="claude" and has an agent state. Shows:
// status chip (color from claude_status), mode chip (agent.mode),
// current-tool with spinner, tokens, cost, elapsed time.
//
// Reads:
//   rootStore.sessions[id].agent — model, mode, tokens, cost, tool
//   rootStore.claude_status[id]  — running / idle / needs-input scrape
//
// No state mutations.

import { createEffect, createMemo, createSignal, onCleanup, Show } from "solid-js";
import { useParams, useLocation } from "@solidjs/router";
import { rootStore } from "../store/root.ts";
import { resolveSessionByFolder } from "../store/selectors.ts";
import { decodeFolderPath } from "../lib/terminalHref.ts";
import "@material/web/progress/circular-progress.js";

// Ticking "now" signal — Date.now() isn't reactive, so the elapsed
// memo would otherwise freeze on first render. Single 1Hz tick
// shared across all consumers via this module-level signal.
const [_now, _setNow] = createSignal(Date.now());
let _nowRefs = 0;
let _nowHandle: ReturnType<typeof setInterval> | null = null;
function _acquireNow(): void {
  _nowRefs++;
  if (_nowHandle !== null) return;
  // Don't churn the reactive graph while hidden; resumes on next tick when visible.
  _nowHandle = setInterval(() => { if (isPageVisible()) _setNow(Date.now()); }, 1000);
}
function _releaseNow(): void {
  _nowRefs--;
  if (_nowRefs > 0 || _nowHandle === null) return;
  clearInterval(_nowHandle);
  _nowHandle = null;
}

function statusColor(status: string | undefined): { bg: string; fg: string; label: string } {
  switch (status) {
    case "needs-input":
      return { bg: "var(--md-tertiary-container)", fg: "var(--md-on-tertiary-container)", label: "needs input" };
    case "running":
      return { bg: "var(--md-primary-container)", fg: "var(--md-on-primary-container)", label: "running" };
    case "running-workflow":
      return { bg: "var(--md-primary-container)", fg: "var(--md-on-primary-container)", label: "workflow" };
    case "idle":
      return { bg: "var(--md-surface-container-high)", fg: "var(--md-on-surface)", label: "idle" };
    default:
      return { bg: "var(--md-surface-container-high)", fg: "var(--md-on-surface-dim)", label: status ?? "—" };
  }
}

function formatElapsed(ms: number): string {
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  return `${h}h${m % 60}m`;
}

function formatCost(usd: number): string {
  if (usd < 0.01) return `$${usd.toFixed(3)}`;
  return `$${usd.toFixed(2)}`;
}

function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k`;
  return `${(n / 1_000_000).toFixed(1)}m`;
}

export function AgentStatusBar() {
  const params = useParams<{ sessionId?: string; workspaceId?: string; channelId?: string; workerFp?: string; folderPath?: string }>();
  const location = useLocation();

  // (1Hz ticker acquire moved below — gated on showBar so a mounted MainPane
  // over a shell-only session doesn't tick a zero-subscriber signal.)

  const activeSessionId = createMemo<string | null>(() => {
    // /t/:workerFp/*folderPath — resolve to the live session in that folder.
    if (params.workerFp && params.folderPath !== undefined) {
      return resolveSessionByFolder(params.workerFp, decodeFolderPath(params.folderPath))?.id ?? null;
    }
    if (params.sessionId) return params.sessionId;
    // /w/:workspaceId/t/:channelId — match by (workspace, channel).
    // Channel numbers are per-worker, NOT globally unique; scoping by
    // workspace_id picks the right session when two workers share a
    // channel index.
    const chId = params.channelId;
    const wsId = params.workspaceId;
    if (!chId || !wsId) return null;
    const ch = parseInt(chId, 10);
    if (Number.isNaN(ch)) return null;
    const s = Object.values(rootStore.sessions).find(
      (x) => x.channel === ch && x.workspace_id === wsId,
    );
    return s?.id ?? null;
  });

  // Hide on /file/ and /search routes.
  const isHidden = createMemo(() =>
    location.pathname.startsWith("/file/") || location.pathname.startsWith("/search"),
  );

  const session = createMemo(() => {
    const id = activeSessionId();
    return id ? rootStore.sessions[id] ?? null : null;
  });
  const agent = createMemo(() => session()?.agent ?? null);
  const status = createMemo(() => {
    const id = activeSessionId();
    return id ? rootStore.claude_status[id] : undefined;
  });

  // Render + ticker gate: bar shows only for a claude session with agent state.
  const showBar = createMemo(
    () => !isHidden() && session()?.kind === "claude" && !!agent(),
  );
  createEffect(() => {
    if (!showBar()) return;
    _acquireNow();
    onCleanup(_releaseNow);
  });

  return (
    <Show when={showBar()}>
      {(_present) => {
        // ALL chip reads are accessor calls (agent()!.x / session()!.x), never a
        // captured `const a = agent()!`: the non-keyed <Show> child runs ONCE, so
        // captured objects froze the stale/mode/tool/cost chips at their mount
        // values (live-correctness bug — tokensIn/Out were already accessors).
        const sc = () => statusColor(status());
        const tokensIn = () => agent()?.tokens?.in ?? 0;
        const tokensOut = () => agent()?.tokens?.out ?? 0;
        const cost = () => agent()?.cost_usd;
        const elapsed = () =>
          formatElapsed(_now() - (session()?.created_at ?? Date.now()));
        return (
          <div class="df-agent-bar" data-testid="agent-status-bar">
            <span
              class="df-agent-chip-status"
              style={{ background: sc().bg, color: sc().fg }}
            >
              <span class="df-agent-chip-dot" />
              {sc().label}
            </span>
            <Show when={agent()?.stale}>
              {/* A1: worker restarted → the bridge that produces live agent
                  state is gone and can't re-attach. Terminal still works;
                  reopening the session respawns a fresh bridge. */}
              <span
                class="df-agent-chip-mode"
                style={{ background: "var(--md-sys-color-surface-container-high)", color: "var(--md-sys-color-on-surface-variant)", opacity: "0.85" }}
                title="Agent state stale after a worker restart — reopen this session to refresh live chips."
              >
                stale · reopen to refresh
              </span>
            </Show>
            <Show when={agent()?.mode}>
              <span class="df-agent-chip-mode">{agent()!.mode}</span>
            </Show>
            <Show when={agent()?.current_tool}>
              <span class="df-agent-tool">
                <md-circular-progress
                  indeterminate
                  aria-label="Tool running"
                  style={{ width: "12px", height: "12px", "--md-circular-progress-active-indicator-color": "var(--md-tertiary)" }}
                />
                <span class="df-agent-tool-name">{agent()!.current_tool!.name}</span>
              </span>
            </Show>
            <div class="df-agent-divider" />
            <span class="df-agent-metric">
              <b>{formatTokens(tokensIn() + tokensOut())}</b>&nbsp;tok
            </span>
            <Show when={typeof cost() === "number" && cost()! > 0}>
              <div class="df-agent-divider" />
              <span class="df-agent-metric"><b>{formatCost(cost()!)}</b></span>
            </Show>
            <div class="df-agent-divider" />
            <span class="df-agent-metric" style={{ color: "var(--md-tertiary)" }}><b>{elapsed()}</b></span>
          </div>
        );
      }}
    </Show>
  );
}
