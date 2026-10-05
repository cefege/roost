//! `roost worker`'s environment handling: what a worker started from a shell
//! inside a Roost session may and may not take from that shell.
//!
//! A Roost PTY carries the five agent-report keys of its session. A worker that
//! read them as its own would bind the installed worker's agent-report socket
//! and receive another worker's reports, so the CLI drops them before resolving.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use roost_cli::daemon::WorkerArgs;
use roost_cli::daemon::worker_boot::resolve_from;
use roost_host::{HostPlatform, MapEnv};

struct TempDir {
    dir: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("roost-worker-boot-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        Self { dir }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_worker_inside_a_roost_session_ignores_that_sessions_agent_endpoint() {
    let scratch = TempDir::new("session-keys");
    let data_dir = scratch.dir.join("data");
    let env = MapEnv::new()
        .with("HOME", scratch.dir.to_str().unwrap())
        .with("ROOST_WORKER_DATA_DIR", data_dir.to_str().unwrap())
        .with(
            "ROOST_WORKER_LOG_DIR",
            scratch.dir.join("logs").to_str().unwrap(),
        )
        .with("ROOST_AGENT_ENDPOINT", "/other.sock")
        .with("ROOST_AGENT_SOCKET_PATH", "/other.sock")
        .with("ROOST_SESSION_ID", "s1");
    let args = WorkerArgs {
        coordinator_url: Some("http://127.0.0.1:4713".to_string()),
    };

    let boot = resolve_from(&args, &env, HostPlatform::Linux).unwrap();

    assert_eq!(boot.agent_report.configured, None);
    assert_eq!(boot.agent_report.data_dir, data_dir);
}
