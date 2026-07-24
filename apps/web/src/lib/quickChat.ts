// Quick chats — ephemeral one-tap AI sessions. A chat is an ordinary session
// spawned into a unique auto-created scratch folder under ~/.roost/chats, so it
// buckets into its own sidebar row for free (folderKeyOf keys on worker+cwd).
// "This bucket is a chat" is a pure path check (isChatFolder). No proto/worker/
// coordinator/DB changes — entirely web-side spawn orchestration + sidebar
// rendering. See quick-chats plan.

import type { Navigator } from "@solidjs/router";
import type { WorkerFp } from "@roost/shared/wire";
import { coordClient } from "../connect.ts";
import { rootStore } from "../store/root.ts";
import { allSessions } from "../store/selectors.ts";
import { workerOnline } from "../store/sync.ts";
import { spawnShell, waitForSession, forceLaunchAgent } from "./spawnSession.ts";
import { addToast } from "./toastStore.ts";

// Tilde path sent to the worker; expandTilde resolves it and mkdirRpc is
// recursive, so one filesMkdir creates ~/.roost/chats/<id> and its parents.
export const CHAT_ROOT = "~/.roost/chats";
// Absolute-path marker for isChatFolder. The store holds the worker-resolved
// absolute cwd (e.g. /Users/mike/.roost/chats/…), which contains this segment
// regardless of the machine's home dir.
export const CHAT_FOLDER_SEGMENT = "/.roost/chats/";

/** True when a folder bucket is a quick-chat scratch dir. A real workspace
 *  whose path contains .roost/chats is vanishingly unlikely; accept it. */
export function isChatFolder(cwd: string): boolean {
  return cwd.includes(CHAT_FOLDER_SEGMENT);
}

// Unique scratch folder per chat: timestamp + 4-char random suffix so two
// clicks within one second never collide.
export function newChatFolderPath(): string {
  const d = new Date();
  const p = (n: number, w = 2) => String(n).padStart(w, "0");
  const stamp = `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}`
    + `-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`;
  const rand = crypto.randomUUID().slice(0, 4);
  return `${CHAT_ROOT}/chat-${stamp}-${rand}`;
}

// Newest session's worker if online, else the first online worker, else null.
// Online-filtered (unlike FlatNewTerminal's /browse heuristic) because a chat
// has no re-pick step — a down Mac would hang the spawn.
export function pickDefaultChatWorker(): WorkerFp | null {
  const recent = [...allSessions()].sort((a, b) => b.created_at - a.created_at)[0];
  if (recent) {
    const w = rootStore.workers[recent.worker_fp];
    if (w && workerOnline(w)) return recent.worker_fp;
  }
  for (const w of Object.values(rootStore.workers)) {
    if (workerOnline(w)) return w.fp;
  }
  return null;
}

// One-tap chat: mkdir scratch → spawn shell → wait for the row → navigate →
// force-launch the selected default agent. Mirrors the non-optimistic doSplit
// pattern (spawn → waitForSession → navigate), not the optimistic doNewTab path.
export async function startQuickChat(navigate: Navigator): Promise<void> {
  const fp = pickDefaultChatWorker();
  if (!fp) { addToast("No machine connected", "err"); return; }
  const folder = newChatFolderPath();
  const sid = crypto.randomUUID();
  try {
    const mk = await coordClient.filesMkdir({ workerFp: fp, path: folder });
    const abs = mk.resolvedPath || folder;
    await spawnShell(fp, abs, sid);
    await waitForSession(sid);
    navigate(`/s/${sid}`);
    forceLaunchAgent(sid);
  } catch (e) {
    // Nothing to unwind — no client placeholder was inserted; a spawned-but-
    // unnavigated PTY is harmless and reachable via its scratch row.
    addToast(`New chat failed: ${e instanceof Error ? e.message : String(e)}`, "err");
  }
}
