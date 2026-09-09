// Terminal-content result presentation for the global search route.
// It joins coordinator matches through the current navigation projection and
// delegates paging and session navigation to its controller-owning parent.

import { createMemo, For, Show } from "solid-js";
import type { SessionsSearchGlobalMatch } from "@roost/shared/proto/coordinator_pb";
import type { GlobalContentSearchController } from "../lib/globalContentSearchController.ts";
import {
  globalContentSearchPartialLabel,
  joinGlobalContentSearchMatches,
} from "../lib/globalContentSearchResults.ts";
import type { NavigationSearchDocument } from "../store/navigation-search.ts";
import {
  Button,
  EmptyState,
  List,
  ListRow,
  Surface,
} from "./Settings/md/primitives.tsx";

interface GlobalSearchContentResultsProps {
  readonly controller: GlobalContentSearchController;
  readonly documents: () => readonly NavigationSearchDocument[];
  readonly query: () => string;
  readonly onOpenResult: (
    document: NavigationSearchDocument,
    match: SessionsSearchGlobalMatch,
  ) => void;
}

export function GlobalSearchContentResults(props: GlobalSearchContentResultsProps) {
  const joinedMatches = createMemo(() =>
    joinGlobalContentSearchMatches(props.controller.matches(), props.documents())
  );
  const documentsBySession = createMemo(() => new Map(
    props.documents().map((document) => [document.sessionId, document]),
  ));
  const missingProjectionMatches = createMemo(() =>
    props.controller.matches().length - joinedMatches().length
  );
  const waiting = createMemo(() => props.controller.debouncing() || props.controller.loading());
  let resultSummaryElement: HTMLDivElement | undefined;
  function retrySearch(): void {
    props.controller.retry();
    queueMicrotask(() => resultSummaryElement?.focus());
  }
  // Sessions past the coordinator's page cap are eligible but never searched,
  // so `searched < eligible` is real missing coverage, not a rounding detail.
  const partialCoverage = createMemo(() => {
    const eligible = props.controller.eligibleSessions();
    return eligible > 0 && props.controller.searchedSessions() < eligible;
  });
  const unsearchedSessions = createMemo(() =>
    Math.max(props.controller.eligibleSessions() - props.controller.searchedSessions(), 0)
  );
  const resultSummary = createMemo(() => {
    if (!props.query().trim()) return "Search retained terminal content with the field above.";
    if (waiting() && !props.controller.hasSearched()) return "Searching retained terminal content…";
    if (props.controller.error()) return `Terminal content search failed: ${props.controller.error()}`;
    const matchCount = joinedMatches().length;
    const searched = props.controller.searchedSessions();
    const eligible = props.controller.eligibleSessions();
    const matchesLabel = `${matchCount} ${matchCount === 1 ? "match" : "matches"}`;
    if (eligible === 0) return matchesLabel;
    return partialCoverage()
      ? `${matchesLabel} across ${searched} of ${eligible} sessions — coverage is partial`
      : `${matchesLabel} across all ${eligible} sessions searched`;
  });
  const incomplete = createMemo(() =>
    props.controller.truncated()
    || props.controller.nextCursor() !== undefined
    || props.controller.partials().length > 0
    || missingProjectionMatches() > 0
    || partialCoverage()
    || (props.controller.error() !== null && joinedMatches().length > 0)
  );

  return (
    <Surface
      as="section"
      level={1}
      radius="lg"
      pad={4}
      aria-labelledby="global-content-search-title"
      data-testid="global-content-search"
      style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-3)" }}
    >
      <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-1)" }}>
        <h2
          id="global-content-search-title"
          style={{
            margin: 0,
            font: "var(--md-title-m-weight) var(--md-title-m-size)/var(--md-title-m-line) var(--md-font)",
          }}
        >
          Terminal content
        </h2>
        <div
          ref={resultSummaryElement}
          tabIndex={-1}
          role="status"
          aria-live="polite"
          aria-atomic="true"
          data-testid="global-content-summary"
          style={{
            color: "var(--md-sys-color-on-surface-variant)",
            font: "var(--md-body-m-weight) var(--md-body-m-size)/var(--md-body-m-line) var(--md-font)",
          }}
        >
          {resultSummary()}
        </div>
      </div>

      <Show when={props.query().trim()}>
        <Show
          when={joinedMatches().length > 0}
          fallback={
            <Show when={!waiting()}>
              <EmptyState
                icon={props.controller.error() ? "error" : "search_off"}
                title={props.controller.error()
                  ? "Terminal content search failed"
                  : incomplete()
                    ? "No matches in the completed portion"
                    : "No terminal content matches"}
                supporting={props.controller.error()
                  ?? (incomplete()
                    ? "Some sessions or retained rows could not be searched."
                    : "No retained terminal row contains this literal query.")}
                action={props.controller.error() && props.controller.retryable()
                  ? <Button variant="tonal" icon="refresh" onClick={retrySearch}>Retry</Button>
                  : undefined}
              />
            </Show>
          }
        >
          <List contained>
            <For each={joinedMatches()}>
              {(result, index) => (
                <ListRow
                  leading="find_in_page"
                  headline={
                    <span data-testid={`global-content-title-${result.document.sessionId}`}>
                      {result.document.displayTitle}
                    </span>
                  }
                  support={
                    <span style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-1)" }}>
                      <span
                        data-testid={`global-content-preview-${result.document.sessionId}-${index()}`}
                        style={{ "white-space": "pre-wrap", "overflow-wrap": "anywhere" }}
                      >
                        {result.match.preview}
                      </span>
                      <span>{result.document.cwd} · {result.document.workerLabel}</span>
                    </span>
                  }
                  trailing="Open find"
                  onClick={() => props.onOpenResult(result.document, result.match)}
                  testId={`global-content-result-${result.document.sessionId}-${index()}`}
                />
              )}
            </For>
          </List>
        </Show>

        <Show when={incomplete()}>
          <Surface
            level={2}
            radius="md"
            pad={3}
            data-testid="global-content-incomplete"
            role={props.controller.error() ? "alert" : "status"}
            aria-live={props.controller.error() ? "assertive" : "polite"}
            aria-atomic="true"
            style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-2)" }}
          >
            <strong>Search incomplete</strong>
            <Show when={props.controller.nextCursor()}>
              <span>More retained terminal rows are available.</span>
            </Show>
            <Show when={partialCoverage()}>
              <span data-testid="global-content-unsearched">
                {unsearchedSessions()} of {props.controller.eligibleSessions()} eligible
                {unsearchedSessions() === 1 ? " session was" : " sessions were"} not searched.
              </span>
            </Show>
            <Show when={props.controller.truncated() && !props.controller.nextCursor()}>
              <span>The bounded page ended before every retained row could be searched.</span>
            </Show>
            <Show when={missingProjectionMatches() > 0}>
              <span>
                {missingProjectionMatches()} {missingProjectionMatches() === 1 ? "match belongs" : "matches belong"} to
                sessions no longer present in the current session list.
              </span>
            </Show>
            <Show when={props.controller.error()}>
              {(message) => <span>{message()}</span>}
            </Show>
            <Show when={props.controller.error() && props.controller.retryable()}>
              <Button variant="tonal" icon="refresh" onClick={retrySearch}>Retry search</Button>
            </Show>
            <For each={props.controller.partials()}>
              {(partial) => (
                <span data-testid={`global-content-partial-${partial.sessionId}`}>
                  {documentsBySession().get(partial.sessionId)?.displayTitle
                    ?? "A session no longer in the current view"} {globalContentSearchPartialLabel(partial.reason)}.
                </span>
              )}
            </For>
          </Surface>
        </Show>

        <Show when={props.controller.nextCursor() && !props.controller.error()}>
          <div style={{ display: "flex", "justify-content": "center" }}>
            <Button
              variant="tonal"
              icon="expand_more"
              disabled={props.controller.loading()}
              onClick={props.controller.loadMore}
              data-testid="global-content-load-more"
            >
              {props.controller.loading() ? "Loading…" : "Load more"}
            </Button>
          </div>
        </Show>
      </Show>
    </Surface>
  );
}
