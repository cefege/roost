//! The two worker-deploy RPCs: which registered worker an operator's target
//! names and which address its deploy dials, the refusals `WorkersDeployStart`
//! answers before anything is spawned, and `WorkersDeployOutput` streaming a
//! job to its end.
//!
//! The target and address cases are ported from
//! `apps/coord/tests/workers/worker-deploy-host.test.ts`; the handler cases pin
//! `apps/coord/src/deploy/handlers-workers-deploy.ts`.
//!
//! `unwrap` and `expect` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to be
//! stated here. Every panic names a value the test just built.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod deploy_support;
mod workers_support;

use connectrpc::ErrorCode;
use deploy_support::{HOST, scripted_job};
use futures_util::StreamExt;
use roost_coord::deploy::rpc_deploy::{
    WorkerDeployRecord, handle_workers_deploy_output, handle_workers_deploy_start,
    resolve_worker_deploy_target, worker_deploy_host,
};
use roost_proto::{WorkersDeployOutputFrame, WorkersDeployOutputRequest, WorkersDeployStartRequest};
use workers_support::{WORKER_FP, WorkersFixture, device_caller, worker_caller};

fn record(fp: &str, os: Option<&str>, label: &str, reachable_addr: Option<&str>) -> WorkerDeployRecord {
    WorkerDeployRecord {
        fp: fp.to_owned(),
        os: os.map(str::to_owned),
        label: label.to_owned(),
        reachable_addr: reachable_addr.map(str::to_owned),
    }
}

// "uses the live reachable address instead of a non-resolvable label", "falls
// back to the registered label when no reachable address exists", "uses the
// authenticated fingerprint for Windows instead of a display label", "keeps an
// explicit unregistered host".
#[test]
fn a_deploy_dials_the_fingerprint_on_windows_else_the_address_then_the_label() {
    let relabelled = record("", None, "mike-m1-air-old", Some("mihai-m1-old.tailnet.ts.net"));
    assert_eq!(
        worker_deploy_host(Some(&relabelled), "mike-m1-air-old"),
        "mihai-m1-old.tailnet.ts.net"
    );
    let unaddressed = record("", None, "linux-worker", None);
    assert_eq!(
        worker_deploy_host(Some(&unaddressed), "worker-fingerprint"),
        "linux-worker"
    );
    let windows = record(&"a".repeat(64), Some("win32"), "Build PC", None);
    assert_eq!(worker_deploy_host(Some(&windows), "Build PC"), "a".repeat(64));
    assert_eq!(
        worker_deploy_host(None, "one-off.tailnet.ts.net"),
        "one-off.tailnet.ts.net"
    );
}

// "rejects duplicate registered labels and reachable names".
#[test]
fn a_label_or_address_two_workers_share_is_refused_as_ambiguous() {
    let workers = [
        record(&"a".repeat(64), Some("win32"), "Shared worker", Some("shared.tail.example")),
        record(&"b".repeat(64), Some("linux"), "Shared worker", Some("shared.tail.example")),
    ];
    for target in ["Shared worker", "shared.tail.example"] {
        assert_eq!(
            resolve_worker_deploy_target(&workers, target),
            Err(format!(
                "ambiguous deploy target \"{target}\" matches multiple registered workers; \
                 use the worker fingerprint"
            ))
        );
    }
}

// "an authenticated fingerprint takes precedence over a colliding display
// label".
#[test]
fn a_fingerprint_wins_over_a_label_that_collides_with_it() {
    let fingerprint = "a".repeat(64);
    let workers = [
        record(&fingerprint, Some("win32"), "Build PC", Some("build-pc.tail.example")),
        record(&"b".repeat(64), Some("win32"), &fingerprint, Some("other.tail.example")),
    ];
    let resolved = resolve_worker_deploy_target(&workers, &fingerprint).unwrap();
    assert_eq!(resolved, Some(&workers[0]));
    assert_eq!(worker_deploy_host(resolved, &fingerprint), fingerprint);
}

async fn deploy_start(
    fixture: &WorkersFixture,
    host: &str,
) -> roost_proto::WorkersDeployStartResponse {
    handle_workers_deploy_start(
        &fixture.core,
        &device_caller(),
        WorkersDeployStartRequest {
            host: host.to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("a deploy start answers in its body")
    .body
}

// `workersDeployStart`: an unknown target, an ambiguous one, and a Windows
// worker are refused in the response body, with no job opened.
#[tokio::test]
async fn deploy_start_refuses_unknown_ambiguous_and_windows_targets_without_a_job() {
    let fixture = WorkersFixture::new("deploy-start-refusals").await;
    fixture.enroll_worker(WORKER_FP, "Shared worker", 1).await;
    fixture
        .enroll_worker(&"c".repeat(64), "Shared worker", 2)
        .await;
    fixture.enroll_worker(&"d".repeat(64), "Build PC", 3).await;
    fixture
        .exec(&format!("UPDATE workers SET os = 'win32' WHERE fp = '{}'", "d".repeat(64)))
        .await;

    for (target, error) in [
        ("nobody.tailnet.ts.net", "worker not found".to_owned()),
        (
            "Shared worker",
            "ambiguous deploy target \"Shared worker\" matches multiple registered workers; \
             use the worker fingerprint"
                .to_owned(),
        ),
    ] {
        let answer = deploy_start(&fixture, target).await;
        assert!(!answer.ok, "{target}");
        assert_eq!(answer.error, error);
        assert!(answer.job_id.is_empty());
    }
    let windows = deploy_start(&fixture, "Build PC").await;
    assert!(!windows.ok);
    assert!(windows.job_id.is_empty());
    assert!(fixture.core.services.deploy.journal().running_hosts().is_empty());
}

// `requireAccountDevice` on both methods: a worker credential is refused.
#[tokio::test]
async fn a_worker_credential_is_refused_by_both_deploy_methods() {
    let fixture = WorkersFixture::new("deploy-worker-caller").await;
    let start = handle_workers_deploy_start(
        &fixture.core,
        &worker_caller(WORKER_FP),
        WorkersDeployStartRequest::default(),
    )
    .await;
    assert_eq!(start.err().map(|error| error.code), Some(ErrorCode::Unauthenticated));
    let output = handle_workers_deploy_output(
        &fixture.core,
        &worker_caller(WORKER_FP),
        &WorkersDeployOutputRequest::default(),
    );
    assert_eq!(output.err().map(|error| error.code), Some(ErrorCode::Unauthenticated));
}

// `workersDeployOutput`: every line as a `line` frame, then one `done` frame
// carrying the exit and the failure, and the stream ends there.
#[tokio::test]
async fn deploy_output_streams_the_job_and_ends_with_its_done_frame() {
    let fixture = WorkersFixture::new("deploy-output").await;
    let journal = fixture.core.services.deploy.journal();
    let job_id = scripted_job(journal, HOST, "echo building; echo activating >&2; exit 7");
    let stream = handle_workers_deploy_output(
        &fixture.core,
        &device_caller(),
        &WorkersDeployOutputRequest {
            job_id: job_id.clone(),
            ..Default::default()
        },
    )
    .expect("the stream opens")
    .body;
    let frames: Vec<WorkersDeployOutputFrame> = stream
        .map(|frame| frame.expect("no frame is an error"))
        .collect()
        .await;
    let (lines, last) = frames.split_at(frames.len() - 1);
    let mut texts: Vec<&str> = lines
        .iter()
        .map(|frame| {
            assert_eq!(frame.kind, "line");
            frame.text.as_str()
        })
        .collect();
    // stdout and stderr are two pipes; only each one's own order is defined.
    texts.sort_unstable();
    assert_eq!(texts, vec!["activating", "building"]);
    assert_eq!(last[0].kind, "done");
    assert_eq!(last[0].exit, 7);
    assert_eq!(last[0].error, "deploy exit 7");

    let unknown = handle_workers_deploy_output(
        &fixture.core,
        &device_caller(),
        &WorkersDeployOutputRequest {
            job_id: "not-a-job".to_owned(),
            ..Default::default()
        },
    )
    .expect("the stream opens")
    .body;
    let frames: Vec<WorkersDeployOutputFrame> =
        unknown.map(|frame| frame.expect("no frame is an error")).collect().await;
    assert_eq!(frames.len(), 1);
    assert_eq!((frames[0].kind.as_str(), frames[0].exit), ("done", -1));
    assert_eq!(frames[0].error, "unknown jobId");
}
