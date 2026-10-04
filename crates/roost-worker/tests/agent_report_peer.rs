//! The kernel, not the caller, names the reporter: a different process that
//! holds a valid session capability is still refused, because its attested
//! peer PID is not the agent the detector sees. The reporter is this test
//! binary re-run as a child, so the PID is a real second process. Mirrors v2
//! `agent-status-report-server.test.ts` "rejects a different peer process …".

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "credential_support/scratch.rs"]
mod scratch;

#[path = "agent_report_support/mod.rs"]
mod agent_report_support;

use std::io::{BufRead, BufReader, Write};
use std::process::Stdio;
use std::sync::Arc;

use agent_report_support::{Detector, Reports, environment, report_line, start};
use roost_worker::agents::BuiltinAgentId;
use scratch::Scratch;
use serde_json::{Value, json};

/// Set only on the child; a harness run of the child test does nothing.
const CHILD_ENDPOINT_ENV: &str = "ROOST_V3_TEST_REPORT_CHILD_ENDPOINT";
const CHILD_BODY_ENV: &str = "ROOST_V3_TEST_REPORT_CHILD_BODY";

/// The libtest name of [`reports_from_a_child_process`] in whichever module
/// path this file was compiled under.
fn child_test_name() -> String {
    let module = module_path!();
    let within_crate = module.split_once("::").map_or(module, |(_, rest)| rest);
    format!("{within_crate}::reports_from_a_child_process")
}

const ANSWER_TAG: &str = "roost-report-answer ";

#[test]
fn reports_from_a_child_process() {
    let Ok(endpoint) = std::env::var(CHILD_ENDPOINT_ENV) else {
        return;
    };
    let body = std::env::var(CHILD_BODY_ENV).expect("the parent passes the request");
    let mut stream = std::os::unix::net::UnixStream::connect(endpoint).expect("the server listens");
    stream
        .write_all(body.as_bytes())
        .expect("the request is written");
    let mut answer = String::new();
    BufReader::new(stream)
        .read_line(&mut answer)
        .expect("an answer arrives");
    println!("{ANSWER_TAG}{}", answer.trim_end());
}

#[tokio::test]
async fn rejects_a_different_peer_process_reporting_for_the_live_agent() {
    let scratch = Scratch::new("agent-report-peer");
    let environment = environment(&scratch);
    let detector = Detector::knowing(BuiltinAgentId::Omp, std::process::id());
    let reports = Arc::new(Reports::default());
    // No injected reader: the kernel's peer credentials attest the reporter.
    let server = start(
        &environment,
        Arc::clone(&detector),
        Arc::clone(&reports),
        Arc::default(),
        None,
    );

    let child = tokio::process::Command::new(std::env::current_exe().expect("a test binary path"))
        .args([
            "--exact",
            &child_test_name(),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENDPOINT_ENV, server.path())
        .env(CHILD_BODY_ENV, report_line(&environment, json!({})))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the child starts");
    let child_pid = child.id().expect("a running child has a pid");
    let output = child.wait_with_output().await.expect("the child exits");
    assert!(output.status.success(), "the child reported: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The harness prints the test name on the same line before `--nocapture`
    // output, so the tag is found anywhere in a line, not only at its start.
    let answer: Value = stdout
        .lines()
        .find_map(|line| line.split_once(ANSWER_TAG).map(|(_, answer)| answer))
        .map(|line| serde_json::from_str(line).expect("a JSON answer"))
        .unwrap_or_else(|| panic!("the child printed no answer: {stdout}"));

    assert_eq!(
        answer,
        json!({ "ok": false, "error": "reporter_identity_mismatch" })
    );
    assert_eq!(*detector.attested.lock().unwrap(), [child_pid]);
    assert!(reports.received.lock().unwrap().is_empty());
    server.close().await;
}
