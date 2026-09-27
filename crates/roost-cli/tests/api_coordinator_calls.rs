//! `roost api` against a coordinator running in this test process, mounted from
//! the generated `CoordinatorService` specs. Owned by the L5 slice.
//!
//! WHAT MAKES THESE BEHAVIOUR TESTS AND NOT WIRING TESTS. Every call leaves
//! this process over HTTP through the generated `CoordinatorServiceClient`, is
//! routed by the spec constant the generator emitted, and comes back decoded
//! into that spec's response type. A verb wired to a hand-typed method path
//! cannot reach this server and every test here fails. Conversely a test that
//! stubbed the client would pass no matter what the verb was wired to — so
//! nothing here stubs it.
//!
//! WHAT IS ASSERTED IS OBSERVABLE. An unknown verb's exit code, the exact
//! contents of stdout for `--json`, the exit code and the named state for a
//! wait, and the difference between a coordinator that answered nothing and a
//! coordinator that answered "nothing to report". None of these read the source
//! and none of them assert that a function was called.

// `clippy.toml`'s `allow-unwrap-in-tests` exempts a `#[test]` body, and the
// command line denies `expect-used` outright. `link_to` and `invocation` are
// ordinary helpers rather than test bodies, so the exemption never reaches
// them and each `expect` is an `expect_used` error the moment clippy runs.
// These panics ARE the assertions: each fires on exactly the value the test
// says must hold.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod api_support;

use std::process::ExitCode;

use roost_cli::api::client::CoordinatorApi;
use roost_cli::api::output::ApiOutput;
use roost_cli::api::verbs::{self, Invocation};
use roost_cli::api::{agent_projection, agents, sessions, workers};
use roost_cli::command_error::GENERIC_FAILURE;
use roost_proto::{AgentPromptWaitOutcome, Session, Worker};

use api_support::{Fixture, SESSION_ID};

/// The two streams, kept apart, because keeping them apart is the contract.
#[derive(Debug, Default)]
struct Captured {
    answers: Vec<String>,
    progress: Vec<String>,
}

impl ApiOutput for Captured {
    fn answer(&mut self, line: &str) {
        self.answers.push(line.to_string());
    }

    fn progress(&mut self, line: &str) {
        self.progress.push(line.to_string());
    }
}

fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn invocation(verb: &str, args: &[&str]) -> Invocation {
    let spec = verbs::lookup(verb).unwrap_or_else(|| panic!("{verb} is registered"));
    verbs::parse(spec, &argv(args)).unwrap_or_else(|failure| panic!("{failure}"))
}

async fn link_to(origin: &str) -> CoordinatorApi {
    CoordinatorApi::at(origin, None).expect("the fixture origin is an http origin")
}

#[tokio::test]
async fn workers_reaches_the_generated_workers_list_method_and_reports_routability() {
    let (origin, server) = api_support::serve(Fixture {
        workers: vec![Worker {
            fp: "a".repeat(64),
            label: "studio".to_string(),
            os: "linux".to_string(),
            ..Default::default()
        }],
        routable_fps: vec!["a".repeat(64)],
        ..Default::default()
    })
    .await;
    let api = link_to(&origin).await;
    let mut out = Captured::default();

    workers::list(&api, &invocation("workers", &[]), &mut out)
        .await
        .expect("the coordinator answered");

    assert_eq!(out.answers[0], "fp\tlabel\tstate\tos");
    assert!(
        out.answers[1].ends_with("\tstudio\tonline\tlinux"),
        "{}",
        out.answers[1]
    );
    server.abort();
}

#[tokio::test]
async fn a_worker_the_coordinator_cannot_route_reads_offline_rather_than_online() {
    let (origin, server) = api_support::serve(Fixture {
        workers: vec![Worker {
            fp: "b".repeat(64),
            label: "asleep".to_string(),
            os: "darwin".to_string(),
            ..Default::default()
        }],
        routable_fps: Vec::new(),
        ..Default::default()
    })
    .await;
    let api = link_to(&origin).await;
    let mut out = Captured::default();

    workers::list(&api, &invocation("workers", &[]), &mut out)
        .await
        .expect("the coordinator answered");

    assert!(
        out.answers[1].ends_with("\tasleep\toffline\tdarwin"),
        "a heartbeat-fresh worker the coordinator cannot route is not online: {}",
        out.answers[1]
    );
    server.abort();
}

#[tokio::test]
async fn sessions_reaches_the_generated_sessions_list_method() {
    let (origin, server) = api_support::serve(Fixture {
        sessions: vec![Session {
            id: SESSION_ID.to_string(),
            worker_fp: "c".repeat(64),
            kind: "shell".to_string(),
            cwd: "/srv/app".to_string(),
            status: "open".to_string(),
            custom_title: Some("deploy".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    })
    .await;
    let api = link_to(&origin).await;
    let mut out = Captured::default();

    sessions::list(&api, &invocation("sessions", &[]), &mut out)
        .await
        .expect("the coordinator answered");

    assert_eq!(out.answers[0], "id\tworker\tkind\tcwd\ttitle");
    assert_eq!(
        out.answers[1],
        format!("{SESSION_ID}\t{}\tshell\t/srv/app\tdeploy", "c".repeat(64))
    );
    server.abort();
}

#[tokio::test]
async fn agent_status_json_is_exactly_one_parseable_document_and_nothing_else() {
    let (origin, server) = api_support::serve(Fixture {
        first_status: Some(Fixture::status("working", 7)),
        ..Default::default()
    })
    .await;
    let api = link_to(&origin).await;
    let mut out = Captured::default();

    let code = agents::status(
        &api,
        &invocation("agent-status", &[SESSION_ID, "--json"]),
        &mut out,
    )
    .await
    .expect("the coordinator answered");

    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out.answers.len(),
        1,
        "stdout carries one document: {:?}",
        out.answers
    );
    let document: serde_json::Value =
        serde_json::from_str(&out.answers[0]).expect("the one stdout line is a JSON document");
    assert_eq!(document["session_id"], SESSION_ID);
    assert_eq!(document["state"], "working");
    assert_eq!(document["revision"], 7);
    assert_eq!(document["promptable"], true);
    server.abort();
}

#[tokio::test]
async fn a_wait_that_reaches_its_state_exits_zero_and_prints_that_state() {
    let (origin, server) = api_support::serve(Fixture {
        first_status: Some(Fixture::status("working", 3)),
        second_status: Some(Fixture::status("blocked", 4)),
        wait_outcome: Some(AgentPromptWaitOutcome::Matched),
        ..Default::default()
    })
    .await;
    let api = link_to(&origin).await;
    let mut out = Captured::default();

    let code = agents::wait(
        &api,
        &invocation(
            "agent-wait",
            &[SESSION_ID, "--until", "blocked", "--timeout", "1s"],
        ),
        &mut out,
    )
    .await
    .expect("the coordinator answered");

    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(out.answers, vec!["blocked".to_string()]);
    server.abort();
}

#[tokio::test]
async fn a_wait_that_times_out_exits_one_and_names_the_state_it_was_still_in() {
    let (origin, server) = api_support::serve(Fixture {
        first_status: Some(Fixture::status("working", 3)),
        second_status: Some(Fixture::status("working", 9)),
        wait_outcome: Some(AgentPromptWaitOutcome::TimedOut),
        ..Default::default()
    })
    .await;
    let api = link_to(&origin).await;
    let mut out = Captured::default();

    let code = agents::wait(
        &api,
        &invocation(
            "agent-wait",
            &[SESSION_ID, "--until", "blocked", "--timeout", "1s"],
        ),
        &mut out,
    )
    .await
    .expect("the coordinator answered");

    // `ExitCode` carries no numeric accessor, so the assertion is against a
    // code built from the same constant `main.rs` would exit with.
    assert_eq!(code, std::process::ExitCode::from(GENERIC_FAILURE));
    assert_eq!(
        out.answers,
        vec!["working".to_string()],
        "a caller that timed out still asked what state the agent is in"
    );
    server.abort();
}

#[tokio::test]
async fn a_wait_reports_the_state_it_was_pinned_to_when_the_occupant_was_replaced() {
    let replaced = Fixture::status_after_replacement("idle", 40);
    let (origin, server) = api_support::serve(Fixture {
        first_status: Some(Fixture::status("working", 3)),
        second_status: Some(replaced),
        wait_outcome: Some(AgentPromptWaitOutcome::OccupantChanged),
        ..Default::default()
    })
    .await;
    let api = link_to(&origin).await;
    let mut out = Captured::default();

    agents::wait(
        &api,
        &invocation(
            "agent-wait",
            &[SESSION_ID, "--until", "idle", "--timeout", "1s"],
        ),
        &mut out,
    )
    .await
    .expect("the coordinator answered");

    assert_eq!(
        out.answers,
        vec!["working".to_string()],
        "a different occupant's state is not the state this wait was parked on"
    );
    server.abort();
}

#[tokio::test]
async fn a_coordinator_that_answers_nothing_is_reported_as_unreachable_not_as_empty() {
    // Port 1 on loopback has nothing listening, which is the case a verb must
    // not turn into an empty table.
    let api = link_to("http://127.0.0.1:1").await;
    let mut out = Captured::default();

    let failure = workers::list(&api, &invocation("workers", &[]), &mut out)
        .await
        .expect_err("nothing is listening");

    assert_eq!(failure.code, GENERIC_FAILURE);
    assert!(
        failure.message.contains("did not answer"),
        "an unreachable coordinator is its own sentence: {}",
        failure.message
    );
    assert!(
        out.answers.is_empty(),
        "an unreachable coordinator must not print an empty fleet: {:?}",
        out.answers
    );
}

#[tokio::test]
async fn no_readout_puts_the_credential_anywhere_near_stdout() {
    let (origin, server) = api_support::serve(Fixture {
        workers: vec![Worker {
            fp: "d".repeat(64),
            label: "studio".to_string(),
            os: "linux".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    })
    .await;
    let api = CoordinatorApi::at(&origin, Some("a-bearer-no-operator-ever-sees"))
        .expect("the fixture origin is an http origin");
    let mut out = Captured::default();

    let code = agents::list(&api, &invocation("agents", &[]), &mut out)
        .await
        .expect("the coordinator answered");
    assert_eq!(code, ExitCode::SUCCESS);
    // `list` prints its header even with nothing to list, so the scan below
    // reads real output. Asserting the header arrived keeps this test from
    // passing without ever having produced a line to scan.
    assert!(!out.answers.is_empty(), "list must print a header: {out:?}");

    for line in out.answers.iter().chain(out.progress.iter()) {
        assert!(
            !line.contains("a-bearer-no-operator-ever-sees"),
            "the credential reached a stream: {line}"
        );
    }
    server.abort();
}

#[test]
fn the_json_projection_publishes_the_fields_a_reader_parses() {
    let projected = agent_projection::project(&Fixture::status("idle", 2)).expect("projectable");
    let document = serde_json::to_value(&projected).expect("serialisable");
    for field in [
        "session_id",
        "agent_id",
        "state",
        "message",
        "status_epoch",
        "occupant_id",
        "source",
        "revision",
        "completed_revision",
        "updated_at",
        "promptable",
    ] {
        assert!(
            document.get(field).is_some(),
            "the published document has no {field}: {document}"
        );
    }
}
