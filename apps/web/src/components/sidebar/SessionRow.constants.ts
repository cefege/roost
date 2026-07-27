// Shared constants + the page-lifetime relative-time ticker for SessionRow.
// Split out of SessionRow.tsx (400-line cap). Re-exported from SessionRow.tsx so
// `import { relTimeTickMs, STAGE_LABEL } from ".../SessionRow.tsx"` (FolderList,
// HomeLanding) resolves unchanged.

import { createSignal } from "solid-js";
import { isPageVisible } from "../../lib/pageVisible.ts";

export const ROW_BASE = { "padding-left": "28px" } as const;

// relTimeTickMs — one page-lifetime 30s ticker shared by every row so relTime
// labels age even when the store is silent, so no lastActivity frame arrives
// to retrigger the memo.
export const [relTimeTickMs, setRelTimeTickMs] = createSignal(Date.now());
// Hidden-tab gate: don't churn the reactive graph while hidden; labels
// catch up on the first tick after the tab is visible again.
setInterval(() => { if (isPageVisible()) setRelTimeTickMs(Date.now()); }, 30_000);

export const STATUS_DOT: Record<string, string> = {
  running: "var(--color-ok)",
  "needs-input": "var(--color-warn)",
  idle: "var(--text-lo)",
  done: "var(--color-info)",
};

// Human-readable OMP stage names for the flat-list status pill.
// Exported so HomeLanding's recent tiles render the same vocabulary.
export const STAGE_LABEL: Record<string, string> = {
  "needs-input": "needs input",
  "running": "running",
  "running-workflow": "running·wf",
  "idle": "idle",
  "done": "done",
  "offline": "offline",
};
