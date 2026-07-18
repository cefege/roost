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
import { liveStatus } from "./attention.ts";

export type Attention = "needs" | "running" | "idle";

export function activityLine(lead: Session, attention: Attention): string {
  if (attention === "needs") {
    if (liveStatus(lead) === "needs-input") return "Waiting on your input";
    // worker present but unreachable → stranded (same guard as needsAttention).
    const w = rootStore.workers[lead.worker_fp];
    if (w && !workerOnline(w)) return "Machine offline — reopen to refresh";
    // idle/done with unseen output — folded into needs by needsAttention().
    const done = lead.agent?.last_message?.text?.trim();
    return done ? done.split("\n")[0] : "Finished";
  }
  const msg = lead.agent?.last_message?.text?.trim();
  if (msg) return msg.split("\n")[0];
  if (attention === "running") {
    const tool = lead.agent?.current_tool?.name;
    return tool ? `${tool}…` : "Working…";
  }
  return ""; // idle + no message → calm, blank subtitle (branch is a chip now)
}
