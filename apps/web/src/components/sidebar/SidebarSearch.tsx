// Controlled sidebar metadata-search input. Cmd-F focuses it; Esc clears it.
// AllView applies the shared navigation-search projection and owns the debounce.
// This component has no store reads and emits every keystroke to its caller.
//
// Debounce policy: this component fires onChange on EVERY keystroke. Callers
// that drive O(n) filters (e.g. AllView over allSessions()) MUST debounce the
// resulting computation themselves — see AllView.SEARCH_DEBOUNCE_MS for the
// canonical pattern.

import type { JSX } from "solid-js";
import { platformShortcutLabel } from "../../lib/browserPlatform.ts";
import { directionalInputActive } from "../../lib/directionalInput.ts";
import { IconButton } from "../Settings/md/IconButton.tsx";

interface SidebarSearchProps {
  query: string;
  onChange: (next: string) => void;
  inputRef?: (el: HTMLInputElement) => void;
  onKeyDown?: (e: KeyboardEvent) => void;
  placeholder?: string;
}

export function SidebarSearch(props: SidebarSearchProps) {
  let searchInput: HTMLInputElement | undefined;
  let searchTrigger: HTMLButtonElement | undefined;

  const setSearchInputRef = (element: HTMLInputElement) => {
    searchInput = element;
    props.inputRef?.(element);
  };
  const handleKeyDown: JSX.EventHandler<HTMLInputElement, KeyboardEvent> = (e) => {
    if (e.key === "Escape" && props.query) {
      e.preventDefault();
      e.stopPropagation();
      props.onChange("");
      return;
    }
    if (e.key === "Escape" && directionalInputActive()) {
      e.preventDefault();
      e.stopPropagation();
      searchTrigger?.focus();
      return;
    }
    props.onKeyDown?.(e as unknown as KeyboardEvent);
  };

  return (
    <div class="df-search workbench-sidebar-search" data-testid="sidebar-search-wrapper">
      <IconButton
        ref={searchTrigger}
        icon="search"
        label="Filter sidebar"
        title="Filter sidebar"
        size="icon-sm"
        class="workbench-sidebar-search__icon"
        data-testid="sidebar-search-trigger"
        onClick={() => searchInput?.focus()}
      />
      <input
        class="workbench-sidebar-search__input"
        ref={setSearchInputRef}
        type="text"
        value={props.query}
        onInput={(event) => props.onChange(event.currentTarget.value)}
        onKeyDown={handleKeyDown}
        placeholder={props.placeholder ?? "Search sessions, workspaces…"}
        aria-label="Filter sidebar"
        data-testid="sidebar-search"
        data-spatial-navigation="manual"
      />
      {props.query ? (
        <IconButton
          icon="close"
          label="Clear search"
          size="icon-sm"
          class="workbench-sidebar-search__clear"
          onClick={() => props.onChange("")}
          title="Clear (Esc)"
          data-testid="sidebar-search-clear"
        />
      ) : (
        <span class="df-search-kbd workbench-sidebar-search__shortcut" aria-hidden="true">
          {platformShortcutLabel("sidebarSearch", "⌘F")}
        </span>
      )}
    </div>
  );
}
