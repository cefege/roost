// Shared constants + the page-lifetime relative-time ticker for SessionRow.
// Split out of SessionRow.tsx so all rows share one page-lifetime ticker.

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
