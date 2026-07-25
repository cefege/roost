// Quick chats — one-tap AI sessions. A chat is a `kind:"agent"` session (web-UI
// mode: its process IS an `omp --mode rpc-ui` child, no PTY) spawned into a
// unique auto-created scratch folder under ~/.roost/chats, so it buckets into
// its own sidebar row for free (folderKeyOf keys on worker+cwd). See quick-chats
// plan.

import type { Navigator } from "@solidjs/router";
import type { WorkerFp } from "@roost/shared/wire";
import { coordClient } from "../connect.ts";
import { rootStore } from "../store/root.ts";
import { allSessions } from "../store/selectors.ts";
import { workerOnline } from "../store/sync.ts";
import { spawnAgent, waitForSession } from "./spawnSession.ts";
import { addToast } from "./toastStore.ts";

// Tilde path sent to the worker; expandTilde resolves it and mkdirRpc is
// recursive, so one filesMkdir creates ~/.roost/chats/<id> and its parents.
export const CHAT_ROOT = "~/.roost/chats";
// Absolute-path marker for isChatFolder. The store holds the worker-resolved
// absolute cwd (e.g. /Users/mike/.roost/chats/…), which contains this segment
// regardless of the machine's home dir.
export const CHAT_FOLDER_SEGMENT = "/.roost/chats/";

/** True when a folder bucket is a quick-chat scratch dir. A real workspace
 *  whose path contains .roost/chats is vanishingly unlikely; accept it.
 *
 *  SIDEBAR GROUPING ONLY — "this bucket was created by the chat button", so the
 *  Chat tab can list it. It is NOT a capability test and never gates behavior:
 *  what a session IS lives in `session.kind` (see ompChatEnabled). */
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

// Candidate machines for a chat, best first: the worker behind the newest
// session, then every other online worker. Online-filtered (unlike
// FlatNewTerminal's /browse heuristic) because a down Mac would hang the spawn.
export function chatWorkerCandidates(): WorkerFp[] {
  const out: WorkerFp[] = [];
  const recent = [...allSessions()].sort((a, b) => b.created_at - a.created_at)[0];
  if (recent) {
    const w = rootStore.workers[recent.worker_fp];
    if (w && workerOnline(w)) out.push(recent.worker_fp);
  }
  for (const w of Object.values(rootStore.workers)) {
    if (workerOnline(w) && !out.includes(w.fp)) out.push(w.fp);
  }
  return out;
}

// One-tap chat: mkdir scratch → spawn an agent session → wait for the row.
//
// No probe: spawnAgent starts the omp child as part of the spawn and FAILS the
// RPC if it cannot (missing binary, unstartable), so the spawn's own success is
// the proof the old awaited `get_state` round trip was buying. A machine that
// cannot serve chat rejects here and the loop moves to the next candidate.
/** Create a quick chat and return its session id. Throws when no candidate
 *  machine can serve one; the caller owns navigation and user-facing errors. */
export async function createQuickChat(): Promise<string> {
  const candidates = chatWorkerCandidates();
  if (candidates.length === 0) throw new Error("No machine connected");

  // One folder name per click, reused across retries: each candidate is a
  // different machine, so a retry cannot collide, and a failed attempt leaves
  // at most one empty scratch dir per machine instead of one per attempt.
  // (No files-delete RPC exists to clean it up; the dir is empty and its
  // session is killed, so no sidebar bucket survives.)
  const folder = newChatFolderPath();
  const tried: string[] = [];
  let lastErr = "";
  for (const fp of candidates) {
    const sid = crypto.randomUUID();
    const host = rootStore.workers[fp]?.label || fp.slice(0, 8);
    tried.push(host);
    try {
      const mk = await coordClient.filesMkdir({ workerFp: fp, path: folder });
      const abs = mk.resolvedPath || folder;
      await spawnAgent(fp, abs, { sessionId: sid });
      await waitForSession(sid);
      return sid;
    } catch (e) {
      lastErr = e instanceof Error ? e.message : String(e);
      // Leave no dead chat row behind on a machine that cannot serve it.
      void coordClient.sessionsKill({ sessionId: sid }).catch(() => { /* already gone */ });
    }
  }
  throw new Error(`No machine can run a chat (tried ${tried.join(", ")}): ${lastErr}`);
}

// UI entry point: create the chat, go to it, and surface any failure as a toast.
export async function startQuickChat(navigate: Navigator): Promise<void> {
  try {
    navigate(`/s/${await createQuickChat()}`);
  } catch (e) {
    addToast(e instanceof Error ? e.message : String(e), "err");
  }
}
