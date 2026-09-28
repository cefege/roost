//! The environment a spawned PTY carries so its agent integration can report:
//! the documented variable names, a per-session capability that survives a
//! worker restart, and the override rules for the endpoint address. Mirrors v2
//! `agent-status-report-server.test.ts` "agent report environment".

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "credential_support/scratch.rs"]
mod scratch;

use std::os::unix::fs::PermissionsExt;

use roost_host::MapEnv;
use roost_worker::agents::environment::{
    AGENT_CAPABILITY_ENV, AGENT_ENDPOINT_ENV, AGENT_ENDPOINT_KIND_ENV, AGENT_SOCKET_PATH_ENV,
    AgentReportEnvironment, AgentReportSite,
};
use roost_worker::shell_spec::SESSION_ID_ENV;
use scratch::Scratch;

const SESSION: &str = "11111111-1111-4111-8111-111111111111";
const OTHER_SESSION: &str = "22222222-2222-4222-8222-222222222222";

fn site(scratch: &Scratch, configured: Option<&str>) -> AgentReportSite {
    AgentReportSite {
        data_dir: scratch.root().to_path_buf(),
        configured: configured.map(str::to_owned),
    }
}

fn variable(overlay: &[(String, String)], name: &str) -> Option<String> {
    overlay.iter().find_map(|(key, value)| (key == name).then(|| value.clone()))
}

#[test]
fn exports_the_report_endpoint_under_the_documented_posix_socket_name() {
    let scratch = Scratch::new("agent-env-names");
    let environment = AgentReportEnvironment::resolve(&site(&scratch, None));
    let overlay = environment.session_overlay(SESSION).expect("the endpoint resolved");
    assert_eq!(variable(&overlay, SESSION_ID_ENV).as_deref(), Some(SESSION));
    assert_eq!(variable(&overlay, AGENT_ENDPOINT_KIND_ENV).as_deref(), Some("uds"));
    let endpoint = variable(&overlay, AGENT_ENDPOINT_ENV).expect("an endpoint");
    assert_eq!(variable(&overlay, AGENT_SOCKET_PATH_ENV), Some(endpoint.clone()));
    assert_eq!(endpoint, scratch.path("agent-report.sock").display().to_string());
}

/// A keeper-surviving agent keeps the capability its PTY was given, so the
/// same session must be minted the same capability by the next worker.
#[test]
fn a_session_capability_survives_a_worker_restart_and_is_per_session() {
    let scratch = Scratch::new("agent-env-restart");
    let capability = |environment: &AgentReportEnvironment, session: &str| {
        variable(&environment.session_overlay(session).unwrap(), AGENT_CAPABILITY_ENV).unwrap()
    };
    let first = AgentReportEnvironment::resolve(&site(&scratch, None));
    let minted = capability(&first, SESSION);
    let mode = std::fs::metadata(scratch.path("agent-report.cap")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "the endpoint secret is this user's alone");

    let restarted = AgentReportEnvironment::resolve(&site(&scratch, None));
    assert_eq!(capability(&restarted, SESSION), minted);
    assert!(restarted.verify_capability(SESSION, &minted));
    assert_ne!(capability(&restarted, OTHER_SESSION), minted);
    assert!(!restarted.verify_capability(OTHER_SESSION, &minted));
}

#[test]
fn an_endpoint_override_must_be_an_absolute_socket_path() {
    let scratch = Scratch::new("agent-env-override");
    let relative = AgentReportEnvironment::resolve(&site(&scratch, Some("agent.sock")));
    let refusal = relative.session_overlay(SESSION).expect_err("a relative override refuses");
    assert_eq!(refusal, "ROOST_AGENT_ENDPOINT must be an absolute UDS path");
    assert!(!relative.verify_capability(SESSION, &"a".repeat(64)));

    let absolute = scratch.path("elsewhere.sock").display().to_string();
    let configured = AgentReportEnvironment::resolve(&site(&scratch, Some(&absolute)));
    let overlay = configured.session_overlay(SESSION).unwrap();
    assert_eq!(variable(&overlay, AGENT_ENDPOINT_ENV), Some(absolute));
}

/// `ROOST_AGENT_ENDPOINT` wins even when empty, and empty means "default".
#[test]
fn the_override_is_read_from_the_boot_environment_with_v2_precedence() {
    let data_dir = std::path::PathBuf::from("/data");
    let read = |env: MapEnv| AgentReportSite::from_env(&env, data_dir.clone()).configured;
    assert_eq!(read(MapEnv::new()), None);
    assert_eq!(
        read(MapEnv::new().with(AGENT_SOCKET_PATH_ENV, "/run/a.sock")),
        Some("/run/a.sock".to_owned())
    );
    let both = MapEnv::new()
        .with(AGENT_ENDPOINT_ENV, "/run/b.sock")
        .with(AGENT_SOCKET_PATH_ENV, "/run/a.sock");
    assert_eq!(read(both), Some("/run/b.sock".to_owned()));
    let empty = MapEnv::new()
        .with(AGENT_ENDPOINT_ENV, "")
        .with(AGENT_SOCKET_PATH_ENV, "/run/a.sock");
    assert_eq!(read(empty), None);
}
