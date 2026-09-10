// New-terminal action for the primary sidebar.
// It preserves the existing browse-first workflow and picks the most recent
// worker before navigating; only its workbench presentation lives here.


import { rootStore } from "../../store/root.ts";
import { allSessions } from "../../store/selectors.ts";
import { useNavigate } from "@solidjs/router";
import { IconButton } from "../Settings/md/IconButton.tsx";

export function FlatNewTerminal() {
  const navigate = useNavigate();
  function open() {
    // Default server = the most-recent session's worker, else the first worker.
    const recent = [...allSessions()].sort((a, b) => b.created_at - a.created_at)[0];
    const fp = recent?.worker_fp ?? Object.values(rootStore.workers)[0]?.fp;
    if (!fp) return; // no machines yet — empty-state CTA covers that case
    navigate(`/browse/${fp}`);
  }

  return (
    <IconButton
      icon="add"
      label="New session"
      class="df-newterm-fab workbench-sidebar-new-terminal"
      data-testid="flat-new-terminal-button"
      onClick={open}
      title="New session"
    />
  );
}
