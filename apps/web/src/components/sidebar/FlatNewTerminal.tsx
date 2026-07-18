// "+ New terminal" for the flat sidebar. One click → /browse/<server> (the
// Google-Drive-style file manager): live folder grid, drill, recents,
// New-folder, Open-terminal-here. Spawn happens on the browse page
// (pickFolder → sessionsSpawn shell). Pre-pointed at the most-recent server.

import { rootStore } from "../../store/root.ts";
import { allSessions } from "../../store/selectors.ts";
import { useNavigate } from "@solidjs/router";

export function FlatNewTerminal() {
  const navigate = useNavigate();
  function open() {
    // Default server = the most-recent session's worker, else the first worker.
    const recent = [...allSessions()].sort((a, b) => b.created_at - a.created_at)[0];
    const fp = recent?.worker_fp ?? Object.values(rootStore.workers)[0]?.fp;
    if (!fp) return; // no machines yet — empty-state CTA covers that case
    navigate(`/browse/${fp}`);
  }

  // M3 circular FAB: icon-only "+" pinned bottom-right of the sidebar, teal
  // primary-container. Opens /browse (the Drive file manager).
  return (
    <button
      type="button"
      class="df-newterm-fab"
      data-testid="flat-new-terminal-button"
      onClick={open}
      aria-label="New session"
      title="New session"
    >
      <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M12 5v14M5 12h14" />
      </svg>
    </button>
  );
}
