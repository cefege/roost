// Route-driven metadata, attention, and terminal-content search across sessions.
// Metadata and content join through one scalar navigation projection; content RPC
// results hand off to pane-local current-epoch find before session navigation.

import { useLocation, useNavigate } from "@solidjs/router";
import { createEffect, createMemo, For, onCleanup, Show } from "solid-js";
import type { SessionsSearchGlobalMatch } from "@roost/shared/proto/coordinator_pb";
import { AGENT_STATUS_PRESENTATION } from "../lib/agentStatus.ts";
import { relTimeSince } from "../lib/relTime.ts";
import { createGlobalContentSearchController } from "../lib/globalContentSearchController.ts";
import { requestTerminalFind } from "../lib/terminalFindIntent.ts";
import {
  attentionNavigationDocuments,
  filterNavigationSearchDocuments,
  navigationSearchDocuments,
  type NavigationSearchDocument,
} from "../store/navigation-search.ts";
import { rootStore } from "../store/root.ts";
import {
  Chip,
  EmptyState,
  List,
  ListRow,
  StatusDot,
  Surface,
  TextField,
} from "./Settings/md/primitives.tsx";
import { GlobalSearchContentResults } from "./GlobalSearchContentResults.tsx";

type SearchScope = "all" | "attention";

export function GlobalSearchPage() {
  const location = useLocation();
  const navigate = useNavigate();
  const routeQuery = createMemo(() => new URLSearchParams(location.search));
  const scope = createMemo<SearchScope>(() =>
    routeQuery().get("scope") === "attention" ? "attention" : "all"
  );
  const query = createMemo(() => routeQuery().get("q") ?? "");
  const caseSensitive = createMemo(() =>
    scope() === "all" && routeQuery().get("case") === "1"
  );
  const searchLabel = createMemo(() =>
    scope() === "attention" ? "Filter attention" : "Search sessions and terminal content"
  );
  const scopedDocuments = createMemo(() => scope() === "attention"
    ? attentionNavigationDocuments(navigationSearchDocuments())
    : navigationSearchDocuments());
  const results = createMemo(() =>
    filterNavigationSearchDocuments(scopedDocuments(), query())
  );
  const metadataSummary = createMemo(() => {
    const count = results().length;
    if (count > 0) return `${count} ${count === 1 ? "session" : "sessions"}`;
    if (query().trim()) return "0 sessions. No matching session metadata";
    return scope() === "attention"
      ? "0 sessions. Nothing needs attention"
      : "0 sessions. No sessions to search";
  });
  const contentSearch = createGlobalContentSearchController();
  const authGeneration = createMemo(() => rootStore.auth_generation);
  createEffect(() => {
    authGeneration();
    if (scope() === "all") contentSearch.setSearch(query(), caseSensitive());
    else contentSearch.setSearch("", false);
  });
  onCleanup(() => contentSearch.dispose());

  const updateRoute = (
    nextScope: SearchScope,
    nextQuery: string,
    nextCaseSensitive = caseSensitive(),
  ): void => {
    const parameters = new URLSearchParams();
    if (nextScope === "attention") parameters.set("scope", "attention");
    if (nextQuery.trim()) parameters.set("q", nextQuery);
    if (nextScope === "all" && nextCaseSensitive) parameters.set("case", "1");
    const serialized = parameters.toString();
    navigate(serialized ? `/search?${serialized}` : "/search", { replace: true });
  };

  const openContentResult = (
    document: NavigationSearchDocument,
    match: SessionsSearchGlobalMatch,
  ): void => {
    requestTerminalFind(document.sessionId, query(), {
      caseSensitive: caseSensitive(),
      preferredGlobalMatch: {
        gridEpoch: match.gridEpoch,
        row: match.row,
        col: match.col,
      },
    });
    navigate(document.href);
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
      <Surface
        as="section"
        level={1}
        radius="lg"
        pad={5}
        border
        style={{
          display: "flex",
          "flex-direction": "column",
          gap: "var(--md-space-4)",
          "flex-shrink": 0,
        }}
      >
        <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-1)" }}>
          <h1 id="global-search-title" class="md-headline-s" style={{ margin: 0 }}>
            Search sessions
          </h1>
          <p
            class="md-body-m"
            style={{ margin: 0, color: "var(--md-sys-color-on-surface-variant)" }}
          >
            Find sessions by metadata and search retained terminal content across every machine.
          </p>
        </div>

        <TextField
          value={query()}
          onInput={(value) => updateRoute(scope(), value)}
          label={searchLabel()}
          placeholder={scope() === "attention" ? "Title, path, agent status…" : "Title, path, or terminal text…"}
          testId="global-search-input"
          autofocus
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
          <Show when={scope() === "all"}>
            <Chip
              label="Match terminal case"
              icon={caseSensitive() ? "check" : "match_case"}
              selected={caseSensitive()}
              onClick={() => updateRoute("all", query(), !caseSensitive())}
              testId="global-search-case-sensitive"
            />
          </Show>
        </div>
      </Surface>

      <div
        style={{
          flex: "1",
          overflow: "auto",
          padding: "var(--md-space-3) var(--md-space-5) var(--md-space-5)",
          "padding-bottom": "calc(var(--md-space-5) + var(--kb-offset))",
          display: "flex",
          "flex-direction": "column",
          gap: "var(--md-space-4)",
        }}
      >
        <Show when={scope() === "all"}>
          <GlobalSearchContentResults
            controller={contentSearch}
            documents={navigationSearchDocuments}
            query={query}
            onOpenResult={openContentResult}
          />
        </Show>
        <Surface
          as="section"
          level={1}
          radius="lg"
          pad={4}
          border
          aria-labelledby="global-search-metadata-title"
          style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-2)" }}
        >
          <h2 id="global-search-metadata-title" class="md-title-m" style={{ margin: 0 }}>
            {scope() === "attention" ? "Agent attention" : "Session metadata"}
          </h2>
          <div
            role="status"
            aria-live="polite"
            aria-atomic="true"
            class="md-label-m"
            style={{ color: "var(--md-sys-color-on-surface-variant)" }}
          >
            {metadataSummary()}
          </div>
          <Show
            when={results().length > 0}
            fallback={
              <EmptyState
                icon={scope() === "attention" ? "notifications_none" : "search_off"}
                title={query().trim()
                  ? scope() === "attention" ? "No matching sessions" : "No matching session metadata"
                  : scope() === "attention"
                    ? "Nothing needs attention"
                    : "No sessions to search"}
                supporting={query().trim()
                  ? scope() === "attention"
                    ? "Try another title, path, workspace, machine, or agent term."
                    : "Terminal content matches appear above. Try another metadata term to filter this list."
                  : scope() === "attention"
                    ? "Blocked agents and unseen completions appear here."
                    : "Sessions appear here as they open."}
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
        </Surface>
      </div>
    </Surface>
  );
}

