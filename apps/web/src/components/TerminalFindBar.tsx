// Find-in-scrollback bar for one terminal pane. Rendered by CellTerminal ABOVE
// the display and INSIDE the pane, so it genuinely consumes rows: the pane's
// ResizeObserver re-claims the smaller viewport and the shell reflows to match.
// That is deliberate — docs/FAILURE-INDEX.md requires a painting pane to have TRUTHFUL
// geometry, so faking or compensating the height is not an option.
//
// State lives in lib/terminalFindController.ts; this file is presentation plus
// the in-bar key handling.

import { Show } from "solid-js";
import { IconButton } from "./Settings/md/primitives.tsx";
import type { TerminalFind } from "../lib/terminalFindController.ts";

export function TerminalFindBar(props: {
  find: TerminalFind;
  /** Alt-screen has no scrollback and hides the painted history, so the search
   *  can only cover what is on screen. Say so rather than implying depth. */
  altScreen: boolean;
  /** Esc / close hands the keyboard back to the PTY. */
  onDismiss: () => void;
}) {
  const find = props.find;
  const counter = () => {
    if (find.matches().length === 0) return find.query() === "" ? "" : "0/0";
    return `${find.index()}/${find.matches().length}${find.truncated() ? "+" : ""}`;
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); props.onDismiss(); return; }
    if (e.key === "Enter") {
      e.preventDefault();
      e.stopPropagation();
      find.step(e.shiftKey ? -1 : 1);
      return;
    }
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "g") {
      e.preventDefault();
      e.stopPropagation();
      find.step(e.shiftKey ? -1 : 1);
    }
  };

  return (
    <div
      class="term-find-bar"
      data-testid="terminal-find-bar"
      role="search"
      onKeyDown={onKeyDown}
    >
      <input
        // FOCUS_OWNERS lists `input`, so the pane's mousedown-PREVENT /
        // keydown-RECOVER guards leave this alone instead of yanking focus to
        // the hidden textarea mid-typing.
        ref={(el) => queueMicrotask(() => el.focus())}
        class="term-find-input"
        data-testid="terminal-find-input"
        data-failed={find.failed() ? "true" : "false"}
        type="text"
        spellcheck={false}
        autocomplete="off"
        aria-label={props.altScreen ? "Find in visible rows" : "Find in scrollback"}
        placeholder={props.altScreen ? "Find (visible rows only)" : "Find in scrollback"}
        value={find.query()}
        onInput={(e) => find.setQuery(e.currentTarget.value)}
      />
      <span class="term-find-count" data-testid="terminal-find-count">{counter()}</span>
      <IconButton
        icon="keyboard_arrow_up"
        label="Previous match"
        data-testid="terminal-find-prev"
        disabled={find.matches().length === 0}
        onClick={() => find.step(-1)}
      />
      <IconButton
        icon="keyboard_arrow_down"
        label="Next match"
        data-testid="terminal-find-next"
        disabled={find.matches().length === 0}
        onClick={() => find.step(1)}
      />
      <button
        type="button"
        class="term-find-toggle"
        data-testid="terminal-find-case"
        data-on={find.caseSensitive() ? "true" : "false"}
        aria-pressed={find.caseSensitive()}
        title="Match case"
        onClick={() => find.toggleCaseSensitive()}
      >
        Aa
      </button>
      <button
        type="button"
        class="term-find-toggle"
        data-testid="terminal-find-regex"
        data-on={find.regex() ? "true" : "false"}
        aria-pressed={find.regex()}
        title="Regular expression"
        onClick={() => find.toggleRegex()}
      >
        .*
      </button>
      <Show when={props.altScreen}>
        <span class="term-find-note">visible rows only</span>
      </Show>
      <IconButton
        icon="close"
        label="Close find"
        data-testid="terminal-find-close"
        onClick={props.onDismiss}
      />
    </div>
  );
}
