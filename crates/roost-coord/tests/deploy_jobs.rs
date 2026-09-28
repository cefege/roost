//! A deploy job's output and its start: the coordinator URL a deployed worker is
//! handed, the `roost deploy` invocation a coordinator-started deploy runs, and
//! the output a reader gets -- buffered lines, live lines, the end at `done`,
//! and the bounded queue a stalled reader trips.
//!
//! The URL cases are ported from `apps/coord/tests/deploy-jobs-url.test.ts`;
//! the invocation case is the Rust guard of the FAILURE-INDEX entry
//! "Coordinator-started worker deploys exit 7 from a detached release worktree".
//! The output cases pin `deployOutput` of `apps/coord/src/deploy/deploy-jobs.ts`
//! against a real subprocess.
//!
//! `unwrap` and `expect` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to be
//! stated here. Every panic names a value the test just built.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod deploy_support;

use std::sync::Arc;

use deploy_support::{FLEET_SHA, HOST, UNHELD_JOB_ID, done, drain, line, scripted_job};
use roost_coord::deploy::jobs::DeployJournal;
use roost_coord::deploy::output_stream::open_deploy_output;
use roost_coord::deploy::start::{
    DeployInvocation, deploy_invocation, resolve_deploy_coordinator_url,
};
use roost_host::{BuildIdentity, MapEnv};

// "prefers the explicit worker target over the declared front doors", "falls
// back through the coordinator identity origin to the front door", "treats a
// declared-but-empty entry as undeclared".
#[test]
fn the_deployed_worker_is_handed_the_first_declared_front_door() {
    let bind = ("ROOST_COORDINATOR_BIND", "127.0.0.1:4103");
    let explicit = MapEnv::new()
        .with(bind.0, bind.1)
        .with("ROOST_COORDINATOR_URL", "https://workers.example.test")
        .with("ROOST_COORDINATOR_PUBLIC_URL", "https://coord.example.test")
        .with("ROOST_WEB_PUBLIC_URL", "https://dashboard.example.test");
    assert_eq!(
        resolve_deploy_coordinator_url(&explicit).as_deref(),
        Some("https://workers.example.test")
    );
    let identity = MapEnv::new()
        .with(bind.0, bind.1)
        .with("ROOST_COORDINATOR_PUBLIC_URL", "https://coord.example.test")
        .with("ROOST_WEB_PUBLIC_URL", "https://dashboard.example.test");
    assert_eq!(
        resolve_deploy_coordinator_url(&identity).as_deref(),
        Some("https://coord.example.test")
    );
    let front_door = MapEnv::new()
        .with(bind.0, bind.1)
        .with("ROOST_WEB_PUBLIC_URL", "https://dashboard.example.test");
    assert_eq!(
        resolve_deploy_coordinator_url(&front_door).as_deref(),
        Some("https://dashboard.example.test")
    );
    let blank = MapEnv::new()
        .with("ROOST_COORDINATOR_PUBLIC_URL", "")
        .with("ROOST_WEB_PUBLIC_URL", "https://dashboard.example.test");
    assert_eq!(
        resolve_deploy_coordinator_url(&blank).as_deref(),
        Some("https://dashboard.example.test")
    );
}

// "refuses when no front door is declared" and "refuses an origin a remote
// worker cannot dial".
#[test]
fn no_front_door_or_one_another_machine_cannot_dial_is_refused() {
    let undeclared = MapEnv::new()
        .with("ROOST_COORDINATOR_BIND", "127.0.0.1:4103")
        .with("ROOST_REACHABLE_ADDR", "coord.tailnet.ts.net");
    assert_eq!(resolve_deploy_coordinator_url(&undeclared), None);
    for url in [
        "http://127.0.0.1:4103",
        "http://localhost:4103",
        "https://mac-mini.local:4102",
        "not-a-url",
    ] {
        let env = MapEnv::new().with("ROOST_COORDINATOR_URL", url);
        assert_eq!(resolve_deploy_coordinator_url(&env), None, "{url}");
    }
}

fn source_build(build_sha: &str) -> BuildIdentity {
    BuildIdentity {
        artifact_version: "dev".to_owned(),
        build_sha: build_sha.to_owned(),
        is_compiled: false,
    }
}

// FAILURE-INDEX "Coordinator-started worker deploys exit 7 from a detached
// release worktree": a coordinator-started deploy always proves its own
// installed release, never the checkout's upstream tip.
#[test]
fn a_coordinator_started_deploy_pins_the_coordinators_own_release() {
    let env = MapEnv::new().with("ROOST_COORDINATOR_URL", "https://workers.example.test");
    assert_eq!(
        deploy_invocation(HOST, None, &source_build(FLEET_SHA), &env),
        Ok(DeployInvocation {
            args: vec![
                "deploy".to_owned(),
                HOST.to_owned(),
                "--coordinator-release".to_owned(),
                format!("--expected-sha={FLEET_SHA}"),
            ],
            coordinator_url: "https://workers.example.test".to_owned(),
        })
    );
    // A `dev` stamp is not a release identity: no deploy may guess one.
    assert_eq!(
        deploy_invocation(HOST, None, &source_build("dev"), &env),
        Err("invalid expected git sha".to_owned())
    );
    assert_eq!(
        deploy_invocation(
            "host; rm -rf /",
            Some(FLEET_SHA),
            &source_build("dev"),
            &env
        ),
        Err("invalid host".to_owned())
    );
}

// `deployOutput` of a running job: the lines written before the reader
// arrived, then each new line, CR stripped and a trailing partial line
// flushed, ending at `done`.
#[tokio::test]
async fn a_reader_gets_the_buffered_and_live_output_and_ends_at_done() {
    let journal = Arc::new(DeployJournal::new());
    let job_id = scripted_job(
        &journal,
        HOST,
        "printf 'one\\r\\n'; sleep 0.3; printf 'two\\nthree'; exit 3",
    );
    let early = open_deploy_output(&journal, &job_id);
    let expected = vec![
        line("one"),
        line("two"),
        line("three"),
        done(Some(3), Some("deploy exit 3")),
    ];
    assert_eq!(drain(early).await, expected);
    // A reader arriving after the end reads the same transcript from the buffer.
    assert_eq!(drain(open_deploy_output(&journal, &job_id)).await, expected);
}

// `deployOutput` of a job id the journal does not hold, or that is not a job
// id at all: one `done` naming the unknown id.
#[tokio::test]
async fn an_unknown_or_malformed_job_id_reads_as_one_done() {
    let journal = DeployJournal::new();
    for job_id in [UNHELD_JOB_ID, "../../etc/passwd", ""] {
        assert_eq!(
            drain(open_deploy_output(&journal, job_id)).await,
            vec![done(None, Some("unknown jobId"))],
            "{job_id:?}"
        );
    }
}

// The `ROOST_SIGNAL` bridge: a known sentinel is lifted into its signal and
// never printed; an unknown one is an ordinary line.
#[tokio::test]
async fn a_known_signal_sentinel_is_lifted_and_an_unknown_one_is_printed() {
    let journal = Arc::new(DeployJournal::new());
    let job_id = scripted_job(
        &journal,
        HOST,
        "echo 'ROOST_SIGNAL deploy.cert_skipped {\"reason\":\"no tailscale\"}'; \
         echo 'ROOST_SIGNAL deploy.cert_skiped'; echo done-line",
    );
    assert_eq!(
        drain(open_deploy_output(&journal, &job_id)).await,
        vec![
            line("ROOST_SIGNAL deploy.cert_skiped"),
            line("done-line"),
            done(Some(0), None),
        ]
    );
}

// sse.ts's bound: a reader that stops reading past the frame cap is ended with
// an overflow instead of buffering the rest, while a reader keeping up is not.
#[tokio::test]
async fn a_stalled_reader_overflows_and_a_live_one_still_reads_to_done() {
    let journal = Arc::new(DeployJournal::new());
    let job = journal.open_job(HOST).unwrap();
    let job_id = job.job_id().to_owned();
    let mut stalled = open_deploy_output(&journal, &job_id);
    for index in 0..600 {
        job.emit_line(&format!("line {index}"));
    }
    journal.finish_job(&job, Some(0), None);

    let overflow = stalled
        .next_message()
        .await
        .expect("a stalled reader is told")
        .expect_err("past 512 queued frames the subscription is over");
    assert_eq!(overflow.frames, 513);
    assert_eq!(stalled.next_message().await, None);

    let keeping_up = drain(open_deploy_output(&journal, &job_id)).await;
    assert_eq!(keeping_up.len(), 601);
    assert_eq!(keeping_up.last(), Some(&done(Some(0), None)));
}
