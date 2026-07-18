// Sidebar search input. Cmd-F focuses it; Esc clears and blurs.
// Filters rootStore.sessions by cwd / agent last_message / workspace name.
// Props: query (string), onChange (setter). Called by SidebarRoot or AllView.
// Depends on: no store reads — pure controlled input; filtering is caller's job.
//
// Debounce policy: this component fires onChange on EVERY keystroke. Callers
// that drive O(n) filters (e.g. AllView over allSessions()) MUST debounce the
// resulting computation themselves — see AllView.SEARCH_DEBOUNCE_MS for the
// canonical pattern.

import type { JSX } from "solid-js";

interface SidebarSearchProps {
  query: string;
  onChange: (next: string) => void;
  inputRef?: (el: HTMLInputElement) => void;
  onKeyDown?: (e: KeyboardEvent) => void;
  placeholder?: string;
}

export function SidebarSearch(props: SidebarSearchProps) {
  const handleKeyDown: JSX.EventHandler<HTMLInputElement, KeyboardEvent> = (e) => {
    if (e.key === "Escape" && props.query) {
      e.preventDefault();
      e.stopPropagation();
      props.onChange("");
      return;
    }
    props.onKeyDown?.(e as unknown as KeyboardEvent);
  };

  return (
    <div class="df-search" data-testid="sidebar-search-wrapper">
      <svg
        width="13" height="13" viewBox="0 0 24 24" fill="none"
        stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"
        style={{ "flex-shrink": 0 }}
        aria-hidden="true"
      >
        <circle cx="11" cy="11" r="8"/><path d="m21 21-4.35-4.35"/>
      </svg>
      <input
        ref={props.inputRef}
        type="text"
        value={props.query}
        onInput={(e) => props.onChange(e.currentTarget.value)}
        onKeyDown={handleKeyDown}
        placeholder={props.placeholder ?? "Search sessions, workspaces…"}
        aria-label="Filter sidebar"
        data-testid="sidebar-search"
      />
      {props.query ? (
        <button
          type="button"
          onClick={() => props.onChange("")}
          title="Clear (Esc)"
          aria-label="Clear search"
          data-testid="sidebar-search-clear"
          style={{
            background: "transparent",
            border: "none",
            cursor: "pointer",
            color: "var(--text-lo)",
            "flex-shrink": 0,
            display: "inline-flex",
            "align-items": "center",
            padding: 0,
            "font-family": "inherit",
          }}
        >
          <svg width="11" height="11" viewBox="0 0 24 24" fill="none"
            stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"
            aria-hidden="true">
            <path d="M18 6 6 18"/><path d="m6 6 12 12"/>
          </svg>
        </button>
      ) : (
        <span class="df-search-kbd" aria-hidden="true">⌘F</span>
      )}
    </div>
  );
}
