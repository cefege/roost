//! Which built-in agent a `ps` row is, and which row of a session's process
//! subtree is its agent. Ports the recognition half of v2
//! `apps/worker/src/agents/process-scan.ts` (`BUILTIN_AGENT_COMMANDS`,
//! `identifyAgentProcess`, `findAgentProcessIdentity`), adapted from Herdr's
//! process-backed detection. Called by `agents::process_scan`. The win32
//! command-line splitter and `.exe` normalisation are not ported (Windows is
//! paused).

use super::BuiltinAgentId;
use super::process_scan::AgentProcessIdentity;
use super::process_tree::{ProcessRecord, agent_foreground_job, process_subtree};

/// Interpreters a JavaScript agent is launched through; the script they run
/// names the agent instead.
const RUNTIME_COMMANDS: [&str; 4] = ["node", "nodejs", "bun", "deno"];

/// v2 `BUILTIN_AGENT_COMMANDS`: every executable name an agent answers to.
pub fn builtin_agent_commands(agent: BuiltinAgentId) -> &'static [&'static str] {
    match agent {
        BuiltinAgentId::Codex => &["codex"],
        BuiltinAgentId::Claude => &["claude", "claude-code"],
        BuiltinAgentId::Gemini => &["gemini"],
        BuiltinAgentId::OpenCode => &["opencode", "open-code"],
        BuiltinAgentId::Cursor => &["cursor-agent"],
        BuiltinAgentId::Amp => &["amp", "amp-local"],
        BuiltinAgentId::Copilot => &["copilot", "github-copilot", "ghcs"],
        BuiltinAgentId::Droid => &["droid"],
        BuiltinAgentId::Grok => &["grok", "grok-build"],
        BuiltinAgentId::Pi => &["pi"],
        BuiltinAgentId::Omp => &["omp"],
    }
}

/// v2 `AGENT_PACKAGE_MARKERS`: path fragments of a runtime-launched script.
fn agent_package_markers(agent: BuiltinAgentId) -> &'static [&'static str] {
    match agent {
        BuiltinAgentId::Codex => &["/@openai/codex/", "/codex/"],
        BuiltinAgentId::Claude => &[
            "/@anthropic-ai/claude-code/",
            "/claude-code/",
            "/.claude/local/",
            "/.local/bin/claude",
        ],
        BuiltinAgentId::Gemini => &["/@google/gemini-cli/", "/gemini-cli/"],
        BuiltinAgentId::OpenCode => &["/opencode/"],
        BuiltinAgentId::Cursor => &["/cursor-agent/"],
        BuiltinAgentId::Amp => &["/@sourcegraph/amp/", "/amp/"],
        BuiltinAgentId::Copilot => &["/@github/copilot/", "/github-copilot/"],
        BuiltinAgentId::Droid => &["/droid/"],
        BuiltinAgentId::Grok => &["/grok-build/", "/grok/"],
        BuiltinAgentId::Pi => &[
            "/@mariozechner/pi-coding-agent/",
            "/@badlogic/pi-coding-agent/",
        ],
        BuiltinAgentId::Omp => &["/@oh-my-pi/pi-coding-agent/", "/oh-my-pi/"],
    }
}

/// v2 `executableName`: strip one leading quote and one trailing quote (with
/// an optional comma), take the basename, and drop a JavaScript extension.
pub fn executable_name(value: &str) -> String {
    let mut clean = value.trim();
    if let Some(rest) = clean.strip_prefix(['"', '\'']) {
        clean = rest;
    }
    let unquoted_tail = clean
        .strip_suffix(',')
        .and_then(|rest| rest.strip_suffix(['"', '\'']))
        .or_else(|| clean.strip_suffix(['"', '\'']));
    if let Some(rest) = unquoted_tail {
        clean = rest;
    }
    let base = match clean.rfind(['/', '\\']) {
        Some(slash) => &clean[slash + 1..],
        None => clean,
    };
    for extension in [".js", ".mjs", ".cjs", ".ts"] {
        if let Some(stem) = base.strip_suffix(extension) {
            return stem.to_owned();
        }
    }
    base.to_owned()
}

/// `^[A-Za-z_][A-Za-z0-9_]*=`: an `env NAME=value` assignment.
fn is_env_assignment(argument: &str) -> bool {
    let Some((name, _)) = argument.split_once('=') else {
        return false;
    };
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|rest| rest.is_ascii_alphanumeric() || rest == '_')
}

/// Strip one leading and one trailing quote (v2 `/^["']|["']$/g`).
fn strip_quotes(value: &str) -> &str {
    let value = value.strip_prefix(['"', '\'']).unwrap_or(value);
    value.strip_suffix(['"', '\'']).unwrap_or(value)
}

/// v2 `identifyAgentProcess`: the agent a row runs, judged by its `comm`, its
/// command (after an `env` prefix), and a runtime-launched script — never by
/// shell command text an argument happens to contain.
pub fn identify_agent_process(record: &ProcessRecord) -> Option<BuiltinAgentId> {
    let argv: Vec<&str> = record.args.split_whitespace().collect();
    let argument = |index: usize| argv.get(index).copied().unwrap_or("");
    let mut command_index = 0;
    if executable_name(argument(0)) == "env" {
        command_index += 1;
        while is_env_assignment(argument(command_index)) {
            command_index += 1;
        }
    }
    let command_name = executable_name(argument(command_index));
    let mut candidates = vec![executable_name(&record.comm), command_name.clone()];
    let mut script_path = "";
    if RUNTIME_COMMANDS.contains(&command_name.as_str()) {
        let mut script_index = command_index + 1;
        while argument(script_index).starts_with('-') {
            script_index += 1;
        }
        script_path = strip_quotes(argument(script_index));
        candidates.push(executable_name(script_path));
    }
    BuiltinAgentId::ALL.into_iter().find(|agent| {
        let commands = builtin_agent_commands(*agent);
        candidates
            .iter()
            .any(|candidate| commands.contains(&candidate.as_str()))
            || (!script_path.is_empty()
                && agent_package_markers(*agent)
                    .iter()
                    .any(|marker| script_path.contains(marker)))
    })
}

/// v2 `findAgentProcessIdentity`: the best agent row in a session's subtree.
/// An exact command name outranks a runtime-launched script, the tty's
/// foreground job outranks a background one, and depth breaks ties.
pub fn find_agent_process_identity(
    records: &[ProcessRecord],
    root_pid: u32,
) -> Option<AgentProcessIdentity> {
    let mut best: Option<(AgentProcessIdentity, usize)> = None;
    for (depth, record) in process_subtree(records, root_pid).into_iter().enumerate() {
        let Some(agent_id) = identify_agent_process(record) else {
            continue;
        };
        let comm = executable_name(&record.comm);
        let exact_command = builtin_agent_commands(agent_id).contains(&comm.as_str());
        let foreground = record.tpgid > 0 && record.pgid == record.tpgid;
        let score = usize::from(exact_command) * 100 + usize::from(foreground) * 50 + depth;
        if best
            .as_ref()
            .is_none_or(|(_, best_score)| score > *best_score)
        {
            best = Some((
                AgentProcessIdentity {
                    agent_id,
                    pid: record.pid,
                    foreground: None,
                },
                score,
            ));
        }
    }
    best.map(|(identity, _)| identity)
}

/// v2 `findExactAgentProcessIdentity`: the identity of exactly `process_id`
/// inside the session's subtree, with the pane's foreground job attached —
/// the only identity a live snapshot row has proved.
pub fn find_exact_agent_process_identity(
    records: &[ProcessRecord],
    root_pid: u32,
    process_id: u32,
) -> Option<AgentProcessIdentity> {
    let tree = process_subtree(records, root_pid);
    let pane_child = tree.first()?;
    let record = tree.iter().find(|candidate| candidate.pid == process_id)?;
    let agent_id = identify_agent_process(record)?;
    Some(AgentProcessIdentity {
        agent_id,
        pid: process_id,
        foreground: Some(agent_foreground_job(records, pane_child, process_id)),
    })
}
