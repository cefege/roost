// Quick chats are ordinary shell sessions in unique scratch folders. The
// sidebar classifies those folders by path; no RPC session kind is involved.

import type { Navigator } from "@solidjs/router";
import type { WorkerFp } from "@roost/shared/wire";
import { coordClient } from "../connect.ts";
import { rootStore } from "../store/root.ts";
import { allSessions } from "../store/selectors.ts";
import { workerOnline } from "../store/sync.ts";
import { spawnShell, waitForSession, forceLaunchAgent } from "./spawnSession.ts";
import { addToast } from "../store/toastStore.ts";

export const CHAT_ROOT = "~/.roost/chats";
export const CHAT_FOLDER_SEGMENT = "/.roost/chats/";

export function isChatFolder(cwd: string): boolean {
  return cwd.includes(CHAT_FOLDER_SEGMENT);
}

export function newChatFolderPath(): string {
  const d = new Date();
  const p = (n: number, width = 2) => String(n).padStart(width, "0");
  const stamp = `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}`
    + `-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`;
  return `${CHAT_ROOT}/chat-${stamp}-${crypto.randomUUID().slice(0, 4)}`;
}

export function pickDefaultChatWorker(): WorkerFp | null {
  const recent = [...allSessions()].sort((a, b) => b.created_at - a.created_at)[0];
  if (recent) {
    const worker = rootStore.workers[recent.worker_fp];
    if (worker && workerOnline(worker)) return recent.worker_fp;
  }
  for (const worker of Object.values(rootStore.workers)) {
    if (workerOnline(worker)) return worker.fp;
  }
  return null;
}

export async function startQuickChat(navigate: Navigator): Promise<void> {
  const fp = pickDefaultChatWorker();
  if (!fp) {
    addToast("No machine connected", "err");
    return;
  }

  const folder = newChatFolderPath();
  const sid = crypto.randomUUID();
  try {
    const mk = await coordClient.filesMkdir({ workerFp: fp, path: folder });
    const abs = mk.resolvedPath || folder;
    await spawnShell(fp, abs, sid);
    await waitForSession(sid);
    navigate(`/s/${sid}`);
    forceLaunchAgent(sid);
  } catch (error) {
    addToast(`New chat failed: ${error instanceof Error ? error.message : String(error)}`, "err");
  }
}
