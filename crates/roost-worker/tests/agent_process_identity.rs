#![cfg(unix)]
//! Process recognition and the shared, throttled process scan behind agent
//! status: which row is an agent, how an identity survives a missed snapshot,
//! and how a forced reporter proof is fenced and aborted. Ports v2
//! `apps/worker/tests/agents/agent-status.test.ts` ("agent process identity").
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use roost_protocol::wire::brand::SessionId;
use roost_worker::agents::BuiltinAgentId as Agent;
use roost_worker::agents::process_identity::{
    builtin_agent_commands, find_agent_process_identity, identify_agent_process,
};
use roost_worker::agents::process_scan::{
    AgentProcessIdentity, AgentProcessScanner, SessionProcessRoot,
};
use roost_worker::agents::process_snapshot::{
    ProcessSnapshotReader, PsSnapshotReader, SNAPSHOT_ABORTED, ScanAbort, parse_ps_snapshot,
};
use roost_worker::agents::process_tree::ProcessRecord;
use roost_worker::uplink::OwnerFuture;

fn record(pid: u32, ppid: u32, comm: &str, args: &str) -> ProcessRecord {
    ProcessRecord {
        pid,
        ppid,
        pgid: 20,
        tpgid: 20,
        comm: comm.to_owned(),
        args: args.to_owned(),
    }
}

fn root() -> ProcessRecord {
    record(10, 1, "bash", "/bin/bash")
}

fn session_root() -> SessionProcessRoot {
    SessionProcessRoot {
        session_id: SessionId::try_from("11111111-1111-4111-8111-111111111111").unwrap(),
        child_pid: 10,
    }
}

fn identity(agent_id: Agent, pid: u32) -> AgentProcessIdentity {
    AgentProcessIdentity {
        agent_id,
        pid,
        foreground: None,
    }
}

/// v2's `readSnapshot` stand-in: the current rows, counted, optionally
/// stalling one read until it is aborted.
#[derive(Default)]
struct ScriptedReader {
    records: Mutex<Vec<ProcessRecord>>,
    reads: AtomicUsize,
    stall_read: Option<usize>,
    stalled_abort: Mutex<Option<ScanAbort>>,
}

impl ScriptedReader {
    fn with(records: Vec<ProcessRecord>) -> Arc<Self> {
        Arc::new(Self {
            records: Mutex::new(records),
            ..Self::default()
        })
    }
}

impl ProcessSnapshotReader for ScriptedReader {
    fn read(&self, abort: ScanAbort) -> OwnerFuture<Result<Vec<ProcessRecord>, String>> {
        let read = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        if self.stall_read == Some(read) {
            *self.stalled_abort.lock().unwrap() = Some(abort);
            return Box::pin(std::future::pending());
        }
        let records = self.records.lock().unwrap().clone();
        Box::pin(async move { Ok(records) })
    }
}

fn scanner(reader: &Arc<ScriptedReader>, throttle: Duration) -> AgentProcessScanner {
    AgentProcessScanner::new(
        Arc::clone(reader) as Arc<dyn ProcessSnapshotReader>,
        throttle,
        tokio::runtime::Handle::current(),
    )
}

#[test]
fn identifies_every_builtin_from_a_live_descendant_command() {
    for agent_id in Agent::ALL {
        let command = builtin_agent_commands(agent_id)[0];
        let child = record(20, 10, command, command);
        assert_eq!(
            find_agent_process_identity(&[root(), child], 10),
            Some(identity(agent_id, 20))
        );
    }
}

#[test]
fn recognizes_runtime_package_launchers_without_reading_shell_command_text() {
    let launcher = record(
        20,
        10,
        "bun",
        "bun /home/me/node_modules/@oh-my-pi/pi-coding-agent/dist/cli.js",
    );
    assert_eq!(identify_agent_process(&launcher), Some(Agent::Omp));
    let shell = record(20, 10, "bash", "/bin/bash -c echo codex gemini omp");
    assert_eq!(identify_agent_process(&shell), None);
}

#[test]
fn recognizes_the_claude_code_process_by_every_identity_it_answers_to() {
    let native = record(20, 10, "claude", "/usr/local/bin/claude");
    assert_eq!(identify_agent_process(&native), Some(Agent::Claude));
    let npm = record(
        21,
        10,
        "node",
        "node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js",
    );
    assert_eq!(identify_agent_process(&npm), Some(Agent::Claude));
    let local_installer = record(
        22,
        10,
        "node",
        "node /home/me/.claude/local/claude-code/cli.js",
    );
    assert_eq!(
        identify_agent_process(&local_installer),
        Some(Agent::Claude)
    );
    let alias = record(23, 10, "claude-code", "claude-code");
    assert_eq!(identify_agent_process(&alias), Some(Agent::Claude));
    let other = record(24, 10, "claude", "claude doctor --verbose fake-args");
    assert_eq!(identify_agent_process(&other), Some(Agent::Claude));
}

#[test]
fn parses_ps_rows_as_v2s_pattern_does() {
    let rows = parse_ps_snapshot(
        "  10     1    10    -1 bash /bin/bash -c  echo  hi\ngarbage\n 11 10 11 11 omp\n",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (rows[0].pid, rows[0].ppid, rows[0].pgid, rows[0].tpgid),
        (10, 1, 10, -1)
    );
    assert_eq!(rows[0].comm, "bash");
    assert_eq!(rows[0].args, "/bin/bash -c  echo  hi");
    assert_eq!(rows[1].args, "");
}

#[tokio::test]
async fn requires_two_consecutive_misses_before_releasing_identity() {
    let reader = ScriptedReader::with(vec![root(), record(20, 10, "codex", "codex")]);
    let scanner = scanner(&reader, Duration::ZERO);
    let roots = [session_root()];
    let id = &session_root().session_id;
    assert_eq!(scanner.scan_agents(&roots).await[id].agent_id, Agent::Codex);
    *reader.records.lock().unwrap() = vec![root()];
    assert_eq!(scanner.scan_agents(&roots).await[id].agent_id, Agent::Codex);
    assert!(!scanner.scan_agents(&roots).await.contains_key(id));
}

#[tokio::test]
async fn requires_a_pre_observed_incumbent_before_admitting_a_reporter() {
    let reader = ScriptedReader::with(vec![
        root(),
        record(20, 10, "pi", "pi"),
        record(30, 20, "omp", "omp"),
    ]);
    let scanner = scanner(&reader, Duration::ZERO);
    let root = session_root();
    assert_eq!(scanner.scan_reporting_agent(&root, 30, None).await, None);
    let scanned = scanner.scan_agents(std::slice::from_ref(&root)).await;
    assert_eq!(scanned[&root.session_id], identity(Agent::Omp, 30));
    let reporter = scanner.scan_reporting_agent(&root, 30, None).await.unwrap();
    assert_eq!((reporter.agent_id, reporter.pid), (Agent::Omp, 30));
    assert_eq!(scanner.scan_reporting_agent(&root, 20, None).await, None);
}

#[tokio::test]
async fn keeps_a_live_incumbent_ahead_of_a_newly_named_descendant_reporter() {
    let incumbent = record(
        20,
        10,
        "bun",
        "bun /home/me/node_modules/@oh-my-pi/pi-coding-agent/dist/cli.js",
    );
    let reader = ScriptedReader::with(vec![root(), incumbent.clone()]);
    let scanner = scanner(&reader, Duration::ZERO);
    let root_of = session_root();
    let roots = [root_of.clone()];
    assert_eq!(
        scanner.scan_agents(&roots).await[&root_of.session_id],
        identity(Agent::Omp, 20)
    );

    *reader.records.lock().unwrap() = vec![root(), incumbent, record(30, 20, "omp", "omp")];
    let kept = &scanner.scan_agents(&roots).await[&root_of.session_id];
    assert_eq!((kept.agent_id, kept.pid), (Agent::Omp, 20));
    assert_eq!(scanner.scan_reporting_agent(&root_of, 30, None).await, None);
    let reporter = scanner
        .scan_reporting_agent(&root_of, 20, None)
        .await
        .unwrap();
    assert_eq!((reporter.agent_id, reporter.pid), (Agent::Omp, 20));
}

#[tokio::test]
async fn forces_a_fresh_snapshot_instead_of_admitting_a_held_screen_identity() {
    let reader = ScriptedReader::with(vec![root(), record(30, 10, "omp", "omp")]);
    let scanner = scanner(&reader, Duration::from_secs(10));
    let root_of = session_root();
    let scanned = scanner.scan_agents(std::slice::from_ref(&root_of)).await;
    assert_eq!(scanned[&root_of.session_id], identity(Agent::Omp, 30));
    *reader.records.lock().unwrap() = vec![root()];

    assert_eq!(scanner.scan_reporting_agent(&root_of, 30, None).await, None);
    assert_eq!(reader.reads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cancels_and_detaches_a_stalled_forced_snapshot() {
    let reader = Arc::new(ScriptedReader {
        records: Mutex::new(vec![root(), record(30, 10, "omp", "omp")]),
        stall_read: Some(2),
        ..ScriptedReader::default()
    });
    let scanner = scanner(&reader, Duration::ZERO);
    let root_of = session_root();
    scanner.scan_agents(std::slice::from_ref(&root_of)).await;
    let abort = ScanAbort::new();
    let forced = tokio::spawn({
        let (scanner, root_of, abort) = (scanner.clone(), root_of.clone(), abort.clone());
        async move {
            scanner
                .scan_reporting_agent(&root_of, 30, Some(abort))
                .await
        }
    });
    while reader.stalled_abort.lock().unwrap().is_none() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    abort.abort();

    assert_eq!(forced.await.unwrap(), None);
    let stalled = reader.stalled_abort.lock().unwrap().clone().unwrap();
    assert!(
        stalled.is_aborted(),
        "the stalled snapshot itself was aborted"
    );
    let reporter = scanner
        .scan_reporting_agent(&root_of, 30, None)
        .await
        .unwrap();
    assert_eq!((reporter.agent_id, reporter.pid), (Agent::Omp, 30));
}

#[tokio::test]
async fn terminates_the_spawned_ps_process_when_its_snapshot_is_aborted() {
    let dir = std::env::temp_dir().join(format!("roost-process-scan-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fake_ps = dir.join("ps");
    std::fs::write(&fake_ps, "#!/bin/sh\nexec /bin/sleep 60\n").unwrap();
    let mut permissions = std::fs::metadata(&fake_ps).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o700);
    std::fs::set_permissions(&fake_ps, permissions).unwrap();

    let abort = ScanAbort::new();
    let reader = PsSnapshotReader::with_program(&fake_ps);
    let snapshot = tokio::spawn({
        let abort = abort.clone();
        async move { reader.read_snapshot(abort).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    abort.abort();
    let result = tokio::time::timeout(Duration::from_secs(5), snapshot)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result, Err(SNAPSHOT_ABORTED.to_owned()));
    std::fs::remove_dir_all(&dir).unwrap();
}
