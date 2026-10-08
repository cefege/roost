#![cfg(windows)]
//! The agent-report server on Windows, over its named pipe: a client connects,
//! the kernel attests its process id, and a request carrying the wrong
//! capability is refused with the same answer the Unix socket gives.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use roost_protocol::wire::brand::SessionId;
use roost_worker::agents::environment::{AgentReportEnvironment, AgentReportSite};
use roost_worker::agents::process_scan::AgentProcessIdentity;
use roost_worker::agents::registry::IntegrationStatusReport;
use roost_worker::agents::report_admission::{IntegrationReportSink, ReportingAgentLookup};
use roost_worker::agents::report_server::{AgentReportServer, AgentReportServerOptions};
use roost_worker::uplink::OwnerFuture;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::ClientOptions;

/// Knows no agent: a refused request never reaches it.
struct NoAgents;

impl ReportingAgentLookup for NoAgents {
    fn reporting_agent_for_session(
        &self,
        _session_id: &SessionId,
        _reporter_pid: u32,
    ) -> OwnerFuture<Option<AgentProcessIdentity>> {
        Box::pin(std::future::ready(None))
    }
}

/// Accepts nothing: a refused request never reaches it either.
struct NoReports;

impl IntegrationReportSink for NoReports {
    fn report_integration(&self, _report: IntegrationStatusReport) -> bool {
        false
    }
}

fn fresh_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let dir =
        std::env::temp_dir().join(format!("roost-report-pipe-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a fresh temp directory");
    dir
}

#[tokio::test]
async fn a_wrong_capability_over_the_named_pipe_is_refused() {
    let data_dir = fresh_dir();
    let environment = Arc::new(AgentReportEnvironment::resolve(&AgentReportSite {
        data_dir: data_dir.clone(),
        configured: None,
    }));
    let server = AgentReportServer::start(AgentReportServerOptions {
        environment: Arc::clone(&environment),
        detector: Arc::new(NoAgents),
        registry: Arc::new(NoReports),
        peer_process_id_reader: None,
        socket_path: None,
    })
    .expect("the report server starts");
    assert!(
        server
            .path()
            .to_string_lossy()
            .starts_with(r"\\.\pipe\roost-"),
        "the endpoint is a named pipe: {}",
        server.path().display()
    );

    let mut client = ClientOptions::new()
        .open(server.path())
        .expect("the pipe accepts a client");
    let wrong = "0".repeat(64);
    let line = format!(
        "{{\"version\":1,\"capability\":\"{wrong}\",\"method\":\"agent.report\",\
         \"params\":{{\"session_id\":\"11111111-1111-4111-8111-111111111111\",\
         \"state\":\"idle\",\"active\":false}}}}\n"
    );
    client
        .write_all(line.as_bytes())
        .await
        .expect("the request is written");
    let mut answer = String::new();
    BufReader::new(client)
        .read_line(&mut answer)
        .await
        .expect("an answer arrives");
    assert_eq!(
        answer.trim_end(),
        r#"{"ok":false,"error":"authentication_failed"}"#
    );

    server.close().await;
    let _ = std::fs::remove_dir_all(&data_dir);
}
