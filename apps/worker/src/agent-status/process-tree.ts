// Process-tree facts read out of one `ps` snapshot: the subtree of a session's
// child pid, and which process group owns the pane tty's foreground job.
// agent-status/process-scan.ts supplies the rows and attaches the foreground
// job to a proved agent identity; agent-prompt-control.ts fences on it.

import { supportedHostPlatform } from "@roost/shared/platform";

const HOST_PLATFORM = supportedHostPlatform();

export interface ProcessRecord {
  pid: number;
  ppid: number;
  pgid: number;
  tpgid: number;
  comm: string;
  args: string;
}

/** The pane tty's foreground job, as far as the agent is concerned. */
export interface AgentForegroundJob {
  /** Foreground process group of the pane tty, read from the pane child's
   *  `tpgid`; 0 when the tty reports no foreground group. */
  groupId: number;
  /** Agent process or descendant that is a member of `groupId`; 0 when the
   *  foreground job belongs entirely outside the agent's subtree. */
  agentMemberPid: number;
}

/** The records rooted at `rootPid`, `rootPid` first, or nothing when the root
 *  is absent from the snapshot. */
export function processSubtree(
  records: readonly ProcessRecord[],
  rootPid: number,
): ProcessRecord[] {
  const children = new Map<number, ProcessRecord[]>();
  for (const record of records) {
    let list = children.get(record.ppid);
    if (!list) children.set(record.ppid, list = []);
    list.push(record);
  }
  const root = records.find((record) => record.pid === rootPid);
  const out: ProcessRecord[] = root ? [root] : [];
  for (let index = 0; index < out.length; index++) {
    const list = children.get(out[index]!.pid);
    if (list) out.push(...list);
  }
  return out;
}

/** Resolve the pane's foreground job: the group id is the pane child's `tpgid`
 *  and membership is `pgid === groupId`. A tool subprocess of the agent counts
 *  as the agent holding the foreground, because a plain fork/exec inherits the
 *  agent's process group. */
export function agentForegroundJob(
  records: readonly ProcessRecord[],
  paneChild: ProcessRecord,
  agentPid: number,
): AgentForegroundJob {
  const groupId = paneChild.tpgid;
  if (groupId <= 0) return { groupId: 0, agentMemberPid: 0 };
  const member = processSubtree(records, agentPid)
    .find((record) => record.pgid === groupId);
  return { groupId, agentMemberPid: member?.pid ?? 0 };
}

/** Whether the agent still owns the terminal it is prompted through. An
 *  interactive child that took the foreground — pager, `$EDITOR`, `sudo`,
 *  nested shell — would otherwise receive the prompt text and its CR instead
 *  of the agent. Windows panes expose no tty foreground group, so the fact is
 *  unprovable and never fenced there. */
export function agentOwnsTerminalForeground(job: AgentForegroundJob | undefined): boolean {
  if (HOST_PLATFORM === "win32") return true;
  return job !== undefined && job.groupId > 0 && job.agentMemberPid > 0;
}
