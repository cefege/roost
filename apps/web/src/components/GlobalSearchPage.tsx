// Route-driven metadata and attention search for current dashboard sessions.
// It consumes the shared navigation projection and navigates only through session hrefs.
// Existing root-store, agent-seen, and terminal-deck owners retain all state and lifecycle authority.

import { useLocation, useNavigate } from "@solidjs/router";
import { createMemo, For, Show } from "solid-js";
import { AGENT_STATUS_PRESENTATION } from "../lib/agentStatus.ts";
import { relTimeSince } from "../lib/relTime.ts";
import {
  attentionNavigationDocuments,
  filterNavigationSearchDocuments,
  navigationSearchDocuments,
} from "../store/navigation-search.ts";
import {
  Chip,
  EmptyState,
  List,
  ListRow,
  StatusDot,
  Surface,
  TextField,
} from "./Settings/md/primitives.tsx";
import "./Settings/md/tokens.css";

type SearchScope = "all" | "attention";

export function GlobalSearchPage() {
  const location = useLocation();
  const navigate = useNavigate();
  const routeQuery = createMemo(() => new URLSearchParams(location.search));
  const scope = createMemo<SearchScope>(() =>
    routeQuery().get("scope") === "attention" ? "attention" : "all"
  );
  const query = createMemo(() => routeQuery().get("q") ?? "");
  const searchLabel = createMemo(() =>
    scope() === "attention" ? "Filter attention" : "Search metadata"
  );
  const scopedDocuments = createMemo(() => scope() === "attention"
    ? attentionNavigationDocuments(navigationSearchDocuments())
    : navigationSearchDocuments());
  const results = createMemo(() =>
    filterNavigationSearchDocuments(scopedDocuments(), query())
  );
  const resultSummary = createMemo(() => {
    const count = results().length;
    if (count > 0) return `${count} ${count === 1 ? "session" : "sessions"}`;
    if (query().trim()) return "0 sessions. No matching sessions";
    return scope() === "attention"
      ? "0 sessions. Nothing needs attention"
      : "0 sessions. No sessions to search";
  });

  const updateRoute = (nextScope: SearchScope, nextQuery: string): void => {
    const parameters = new URLSearchParams();
    if (nextScope === "attention") parameters.set("scope", "attention");
    if (nextQuery.trim()) parameters.set("q", nextQuery);
    const serialized = parameters.toString();
    navigate(serialized ? `/search?${serialized}` : "/search", { replace: true });
  };

  return (
    <Surface
      as="section"
      level={0}
      aria-labelledby="global-search-title"
      data-testid="global-search-page"
      style={{
        flex: "1",
        display: "flex",
        "flex-direction": "column",
        overflow: "hidden",
        color: "var(--md-sys-color-on-surface)",
      }}
    >
      <div
        style={{
          display: "flex",
          "flex-direction": "column",
          gap: "var(--md-space-4)",
          padding: "var(--md-space-5)",
          "border-bottom": "1px solid var(--md-sys-color-outline-variant)",
          "flex-shrink": 0,
        }}
      >
        <div>
          <h1
            id="global-search-title"
            style={{
              margin: 0,
              font: "var(--md-headline-s-weight) var(--md-headline-s-size)/var(--md-headline-s-line) var(--md-font)",
            }}
          >
            Search sessions
          </h1>
          <div
            style={{
              color: "var(--md-sys-color-on-surface-variant)",
              font: "var(--md-body-m-weight) var(--md-body-m-size)/var(--md-body-m-line) var(--md-font)",
            }}
          >
            Find sessions by title, folder, workspace, machine, Git, pull request, or port metadata.
          </div>
        </div>

        <TextField
          value={query()}
          onInput={(value) => updateRoute(scope(), value)}
          label={searchLabel()}
          placeholder="Title, path, branch, machine…"
          testId="global-search-input"
          autofocus
          style={{ width: "100%" }}
        />

        <div
          role="group"
          aria-label="Search scope"
          style={{ display: "flex", gap: "var(--md-space-2)", "flex-wrap": "wrap" }}
        >
          <Chip
            label="All sessions"
            icon={scope() === "all" ? "check" : "terminal"}
            selected={scope() === "all"}
            onClick={() => updateRoute("all", query())}
            testId="global-search-scope-all"
          />
          <Chip
            label="Needs attention"
            icon={scope() === "attention" ? "check" : "notifications"}
            selected={scope() === "attention"}
            onClick={() => updateRoute("attention", query())}
            testId="global-search-scope-attention"
          />
        </div>
      </div>

      <div style={{
        flex: "1",
        overflow: "auto",
        padding: "var(--md-space-3) var(--md-space-5) var(--md-space-5)",
        "padding-bottom": "calc(var(--md-space-5) + max(var(--kb-offset), 0px))",
      }}>
        <div
          role="status"
          aria-live="polite"
          aria-atomic="true"
          style={{
            color: "var(--md-sys-color-on-surface-variant)",
            font: "var(--md-label-m-weight) var(--md-label-m-size)/var(--md-label-m-line) var(--md-font)",
            "margin-bottom": "var(--md-space-2)",
          }}
        >
          {resultSummary()}
        </div>
        <Show
          when={results().length > 0}
          fallback={
            <EmptyState
              icon={scope() === "attention" ? "notifications_none" : "search_off"}
              title={query().trim()
                ? "No matching sessions"
                : scope() === "attention"
                  ? "Nothing needs attention"
                  : "No sessions to search"}
              supporting={query().trim()
                ? "Try another title, path, workspace, machine, Git, pull request, or port term."
                : scope() === "attention"
                  ? "Blocked agents and unseen completions appear here."
                  : "Sessions in this dashboard appear here as they open."}
            />
          }
        >
          <div
            data-testid="global-search-results"
            style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-2)" }}
          >
            <List contained>
              <For each={results()}>
                {(document) => {
                  const metadata = [
                    document.workspaceName,
                    document.workerLabel,
                    document.gitBranch ? `branch ${document.gitBranch}` : null,
                    document.gitRemote,
                    document.pullRequestNumber === null ? null : `PR #${document.pullRequestNumber}`,
                    document.portLabel,
                  ].filter((value): value is string => value !== null).join(" · ");
                  const attentionLabel = document.agentAttention
                    ? AGENT_STATUS_PRESENTATION[document.agentAttention].label
                    : null;
                  return (
                    <ListRow
                      leading="terminal"
                      headline={
                        <span data-testid={`global-search-title-${document.sessionId}`}>
                          {document.displayTitle}
                        </span>
                      }
                      support={
                        <span style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-1)" }}>
                          <span>{document.cwd}</span>
                          <Show when={metadata}>
                            <span>{metadata}</span>
                          </Show>
                          <Show when={document.agentMessage}>
                            {(message) => <span>{message()}</span>}
                          </Show>
                        </span>
                      }
                      trailing={
                        <span
                          style={{
                            display: "flex",
                            "align-items": "center",
                            gap: "var(--md-space-2)",
                            color: "var(--md-sys-color-on-surface-variant)",
                            font: "var(--md-label-m-weight) var(--md-label-m-size)/var(--md-label-m-line) var(--md-font)",
                            "white-space": "nowrap",
                          }}
                        >
                          <Show when={attentionLabel}>
                            <span data-testid={`global-search-attention-${document.sessionId}`}>
                              {attentionLabel}
                            </span>
                          </Show>
                          <StatusDot
                            status={document.available ? "ok" : "offline"}
                            title={document.available ? "Available" : "Machine unavailable"}
                          />
                          <span data-testid={`global-search-availability-${document.sessionId}`}>
                            {document.available ? relTimeSince(document.activityAt) : "Unavailable"}
                          </span>
                        </span>
                      }
                      href={document.href}
                      testId={`global-search-result-${document.sessionId}`}
                    />
                  );
                }}
              </For>
            </List>
          </div>
        </Show>
      </div>
    </Surface>
  );
}

