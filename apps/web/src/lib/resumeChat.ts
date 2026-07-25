// Resume an existing omp conversation in a web-UI (kind:"agent") session.
//
// This is the capability the deleted mirror engine existed for — "I started in
// a terminal, continue on my phone" — without any live coupling. The transcript
// is read ONCE, at spawn, by the omp child itself (`--session FILE`). Roost
// never tails it, never attaches to the terminal that wrote it, and nothing
// about the resumed session differs from a fresh one afterwards.
//
// The session lands in the transcript's OWN cwd, not a ~/.roost/chats scratch
// dir: the conversation is about that project, and its file mentions and tool
// calls are relative to it.

import type { Navigator } from "@solidjs/router";
import type { WorkerFp } from "@roost/shared/wire";
import { coordClient } from "../connect.ts";
import { spawnAgent, waitForSession } from "./spawnSession.ts";
import { addToast } from "./toastStore.ts";

export interface OmpTranscript {
  path: string;
  cwd: string;
  title: string;
  updatedAt: number;
  lastPrompt: string;
  /** A live omp is probably still writing this file. Two writers corrupt a
   *  session file, so the worker REFUSES to resume one — the picker disables it
   *  rather than letting the user find out via an error toast. */
  active: boolean;
}

/** Resumable omp transcripts on one machine, newest first. */
export async function listOmpTranscripts(workerFp: WorkerFp, limit = 30): Promise<OmpTranscript[]> {
  const r = await coordClient.sessionsListOmpSessions({ workerFp, limit });
  return r.sessions.map((s) => ({
    path: s.path,
    cwd: s.cwd,
    title: s.title,
    updatedAt: Number(s.updatedAt),
    lastPrompt: s.lastPrompt,
    active: s.active,
  }));
}

/** Spawn an agent session continuing `t`, and return its session id. */
export async function resumeOmpTranscript(workerFp: WorkerFp, t: OmpTranscript): Promise<string> {
  const sid = crypto.randomUUID();
  await spawnAgent(workerFp, t.cwd || "~", { sessionId: sid, resumeSessionFile: t.path });
  await waitForSession(sid);
  return sid;
}

/** UI entry point: resume, navigate, and surface any failure as a toast. The
 *  worker's refusal message ("that omp session looks active…") reaches the user
 *  verbatim — it names the fix. */
export async function startResumedChat(navigate: Navigator, workerFp: WorkerFp, t: OmpTranscript): Promise<void> {
  try {
    navigate(`/s/${await resumeOmpTranscript(workerFp, t)}`);
  } catch (e) {
    addToast(e instanceof Error ? e.message : String(e), "err");
  }
}
