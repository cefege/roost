// Folder-row subtitle — the ONE context line under a folder's headline.
// Pure read of the store (liveStatus + worker routability); lives here (not in
// FolderList.tsx) so it's unit-testable without the Solid JSX transform.
// Live activity when an agent works, the needs reason when it wants
// you, else empty. The git branch is NOT dumped here anymore — it reads as
// unlabeled mystery text ("looks like a branch, no idea what for"); it's a
// self-labeled ⎇ chip on the supporting line instead. Consumed by
// FolderList.tsx groups() → the row's df-flat-subtitle.

import type { Session } from "@roost/shared/wire";
import { rootStore } from "../store/root.ts";
import { workerOnline } from "../store/sync.ts";
import { liveStatus, latestAssistantOutput } from "./attention.ts";

export type Attention = "needs" | "running" | "idle";

export function activityLine(lead: Session, attention: Attention): string {
  if (attention === "needs") {
    if (liveStatus(lead) === "needs-input") return "Waiting on your input";
    // worker present but unreachable → stranded (same guard as needsAttention).
    const w = rootStore.workers[lead.worker_fp];
    if (w && !workerOnline(w)) return "Machine offline — reopen to refresh";
    // idle output that arrived after the user last looked.
    const output = latestAssistantOutput(lead)?.text;
    return output ? output.split("\n")[0] : "Finished";
  }
  const output = latestAssistantOutput(lead)?.text;
  if (output) return output.split("\n")[0];
  if (attention === "running") return "Working…";
  return ""; // idle + no message → calm, blank subtitle (branch is a chip now)
}
