//! Process-tree facts read out of one `ps` snapshot: the subtree of a session's
//! child pid, and which process group owns the pane tty's foreground job. Ports
//! v2 `apps/worker/src/agents/process-tree.ts`. `agents::process_scan` supplies
//! the rows and attaches the foreground job to a proved agent identity;
//! `agents::prompt_control` fences a prompt on it. A Windows ConPTY pane has no
//! tty foreground group, so there the fence cannot be proved and is not held.

use std::collections::HashMap;

/// One `ps -A -o pid=,ppid=,pgid=,tpgid=,comm=,args=` row.
///
/// `pgid` and `tpgid` are signed because `ps` reports `-1` for a process with
/// no controlling terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRecord {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: i32,
    pub tpgid: i32,
    pub comm: String,
    pub args: String,
}

/// The pane tty's foreground job, as far as the agent is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentForegroundJob {
    /// Foreground process group of the pane tty, read from the pane child's
    /// `tpgid`; 0 when the tty reports no foreground group.
    pub group_id: i32,
    /// Agent process or descendant that is a member of `group_id`; 0 when the
    /// foreground job belongs entirely outside the agent's subtree.
    pub agent_member_pid: u32,
}

/// The records rooted at `root_pid`, `root_pid` first (breadth-first, in
/// snapshot order), or nothing when the root is absent from the snapshot.
pub fn process_subtree(records: &[ProcessRecord], root_pid: u32) -> Vec<&ProcessRecord> {
    let mut children: HashMap<u32, Vec<&ProcessRecord>> = HashMap::new();
    for record in records {
        children.entry(record.ppid).or_default().push(record);
    }
    let mut out: Vec<&ProcessRecord> = records
        .iter()
        .find(|record| record.pid == root_pid)
        .into_iter()
        .collect();
    let mut index = 0;
    while index < out.len() {
        if let Some(list) = children.get(&out[index].pid) {
            out.extend(list.iter().copied());
        }
        index += 1;
    }
    out
}

/// Resolve the pane's foreground job: the group id is the pane child's `tpgid`
/// and membership is `pgid == group_id`. A tool subprocess of the agent counts
/// as the agent holding the foreground, because a plain fork/exec inherits the
/// agent's process group.
pub fn agent_foreground_job(
    records: &[ProcessRecord],
    pane_child: &ProcessRecord,
    agent_pid: u32,
) -> AgentForegroundJob {
    let group_id = pane_child.tpgid;
    if group_id <= 0 {
        return AgentForegroundJob {
            group_id: 0,
            agent_member_pid: 0,
        };
    }
    let member = process_subtree(records, agent_pid)
        .into_iter()
        .find(|record| record.pgid == group_id);
    AgentForegroundJob {
        group_id,
        agent_member_pid: member.map_or(0, |record| record.pid),
    }
}

/// Whether the agent still owns the terminal it is prompted through. An
/// interactive child that took the foreground — pager, `$EDITOR`, `sudo`,
/// nested shell — would otherwise receive the prompt text and its CR instead
/// of the agent. An identity with no live snapshot proof is unproved.
#[cfg(unix)]
pub fn agent_owns_terminal_foreground(job: Option<&AgentForegroundJob>) -> bool {
    job.is_some_and(|job| job.group_id > 0 && job.agent_member_pid > 0)
}

/// A ConPTY pane has no foreground process group to prove ownership against,
/// so the fence answers yes rather than refusing every prompt on Windows.
#[cfg(windows)]
pub fn agent_owns_terminal_foreground(_job: Option<&AgentForegroundJob>) -> bool {
    true
}
