//! Rpi session activation admission and machine-local catalog boundaries.
//! Uses the migrated coordinator, real worker dispatcher and durable commands.
//! No provider credentials or model generation are required by these checks.

mod agent_fixture;
mod db_support;

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use agent_fixture::{AgentFixture, WORKER_A};
use connectrpc::ErrorCode;
use roost_coord::{
    agent::rpi::rpc::{handle_agent_rpi_models_list, handle_agent_rpi_session_open},
    coord_core::worker_handle::WorkerHandle,
    worker_link::dispatch::{DispatchOutcome, FrameClass, FrameDispatch, InboundFrame},
};
use roost_proto as proto;
use roost_protocol::{
    versioning::CAPABILITY_RPI_AGENT_V1,
    wire::{
        WorkerFp,
        coord_worker::{CoordWorkerDownstream as Down, CoordWorkerUpstream as Up},
    },
};
use tokio::sync::mpsc;

fn register(
    fixture: &AgentFixture,
    capable: bool,
) -> (Arc<WorkerHandle>, mpsc::UnboundedReceiver<Down>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    let capabilities = if capable {
        BTreeSet::from([CAPABILITY_RPI_AGENT_V1.into()])
    } else {
        BTreeSet::new()
    };
    let worker = Arc::new(WorkerHandle::new(
        WorkerFp::try_from(WORKER_A.to_owned()).unwrap(),
        Some("process-a".into()),
        "connection-a".into(),
        capabilities,
        Arc::new(move |frame| i64::from(sender.send(frame).is_ok())),
    ));
    worker.mark_ready();
    fixture.core.services.workers.insert(Arc::clone(&worker));
    (worker, receiver)
}

async fn seed(fixture: &AgentFixture, id: &str, backend: &str) {
    sqlx::query("INSERT INTO agent_conversations(id,title,worker_fp,worker_label,worker_os,cwd,model_provider,model_id,thinking_level,mode,run_state,created_ms,updated_ms,backend) VALUES($1,'saved',$2,'worker','linux','/work','local','coder','off','normal','idle',1000,1000,$3)")
        .bind(id).bind(WORKER_A).bind(backend).execute(fixture.database().pool()).await.unwrap();
    if backend == "rpi" {
        let transcript = roost_coord::agent::omp::store::empty_transcript();
        sqlx::query("INSERT INTO rpi_sessions(conversation_id,transcript_json) VALUES($1,$2)")
            .bind(id)
            .bind(serde_json::to_string(&transcript).unwrap())
            .execute(fixture.database().pool())
            .await
            .unwrap();
    }
}

fn open(id: &str, request_id: &str, resume_pending: bool) -> proto::AgentRpiSessionOpenRequest {
    proto::AgentRpiSessionOpenRequest {
        conversation_id: id.into(),
        request_id: request_id.into(),
        resume_pending,
        ..Default::default()
    }
}

async fn receive(receiver: &mut mpsc::UnboundedReceiver<Down>) -> Down {
    tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn rpi_contract_open_is_device_only_and_archived_identity_is_indistinguishable_from_missing()
{
    let fixture = AgentFixture::new("rpi-open-refusal").await;
    seed(&fixture, "archive", "native").await;
    let error = handle_agent_rpi_session_open(
        &fixture.core,
        &fixture.worker_caller,
        open("archive", "open", false),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Unauthenticated);
    for id in ["archive", "missing"] {
        let error =
            handle_agent_rpi_session_open(&fixture.core, &fixture.caller, open(id, "open", false))
                .await
                .unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
        assert_eq!(error.message.as_deref(), Some("conversation not found"));
    }
    let error = handle_agent_rpi_models_list(
        &fixture.core,
        &fixture.worker_caller,
        proto::AgentRpiModelsListRequest {
            worker_fp: WORKER_A.into(),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Unauthenticated);
}

#[tokio::test]
async fn rpi_contract_concurrent_open_retries_admit_once_and_cannot_change_resume_intent() {
    let fixture = AgentFixture::new("rpi-open-dedup").await;
    seed(&fixture, "active", "rpi").await;
    let (_worker, mut receiver) = register(&fixture, true);
    let (first, second) = tokio::join!(
        handle_agent_rpi_session_open(
            &fixture.core,
            &fixture.caller,
            open("active", "open-1", false)
        ),
        handle_agent_rpi_session_open(
            &fixture.core,
            &fixture.caller,
            open("active", "open-1", false)
        ),
    );
    first.unwrap();
    second.unwrap();
    let Down::RpiCommand(first) = receive(&mut receiver).await else {
        panic!("OPEN frame")
    };
    let Down::RpiCommand(second) = receive(&mut receiver).await else {
        panic!("OPEN retry frame")
    };
    assert_eq!(first, second);
    assert_eq!(first.command_seq, 1);
    let stored: (i64, String) = sqlx::query_as("SELECT command_seq,delivery_state FROM rpi_commands WHERE conversation_id='active' AND request_id='open-1'")
        .fetch_one(fixture.database().pool()).await.unwrap();
    assert_eq!(stored, (1, "pending".into()));
    let next: i64 = sqlx::query_scalar(
        "SELECT next_command_seq FROM rpi_sessions WHERE conversation_id='active'",
    )
    .fetch_one(fixture.database().pool())
    .await
    .unwrap();
    assert_eq!(next, 1);
    let error = handle_agent_rpi_session_open(
        &fixture.core,
        &fixture.caller,
        open("active", "open-1", true),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    let prompt_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM rpi_commands WHERE kind='submit'")
            .fetch_one(fixture.database().pool())
            .await
            .unwrap();
    assert_eq!(prompt_count, 0, "opening a view never submits a prompt");
}

#[tokio::test]
async fn rpi_contract_catalog_requires_capability_and_current_process_reply() {
    let fixture = AgentFixture::new("rpi-catalog-fence").await;
    let (_old, _receiver) = register(&fixture, false);
    let request = || proto::AgentRpiModelsListRequest {
        worker_fp: WORKER_A.into(),
        ..Default::default()
    };
    let error = handle_agent_rpi_models_list(&fixture.core, &fixture.caller, request())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::FailedPrecondition);
    let (worker, mut receiver) = register(&fixture, true);
    let dispatcher = fixture.core.services.worker_dispatcher(worker);
    let (response, ()) = tokio::join!(
        handle_agent_rpi_models_list(&fixture.core, &fixture.caller, request()),
        async {
            let Down::RpiModelsList(request) = receive(&mut receiver).await else {
                panic!("catalog frame")
            };
            let reply = |epoch: &str| InboundFrame {
                class: FrameClass::Rpc,
                channel: 0,
                frame: Up::RpiModelsListResult(proto::WRpiModelsListResult {
                    request_id: request.request_id.clone(),
                    worker_epoch: epoch.into(),
                    models: vec![proto::RpiModel {
                        provider: "local".into(),
                        model_id: "coder".into(),
                        name: "Coder".into(),
                        supports_images: true,
                        thinking_levels: vec!["off".into()],
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            };
            assert_eq!(
                dispatcher.handle_now(WORKER_A, reply("old-process")),
                DispatchOutcome::Refused
            );
            assert_eq!(
                fixture.core.services.scrollback.pending().pending_count(),
                1
            );
            assert_eq!(
                dispatcher.handle_now(WORKER_A, reply("process-a")),
                DispatchOutcome::Handled
            );
        }
    );
    let models = response.unwrap().body.models;
    assert_eq!(models[0].provider, "local");
    assert_eq!(models[0].model_id, "coder");
    assert!(models[0].supports_images);
    assert_eq!(models[0].thinking_levels, ["off"]);
}

#[tokio::test]
async fn rpi_contract_catalog_failure_never_becomes_empty_success_or_leaks_provider_text() {
    let fixture = AgentFixture::new("rpi-catalog-error").await;
    let (worker, mut receiver) = register(&fixture, true);
    let dispatcher = fixture.core.services.worker_dispatcher(worker);
    let (response, ()) = tokio::join!(
        handle_agent_rpi_models_list(
            &fixture.core,
            &fixture.caller,
            proto::AgentRpiModelsListRequest {
                worker_fp: WORKER_A.into(),
                ..Default::default()
            }
        ),
        async {
            let Down::RpiModelsList(request) = receive(&mut receiver).await else {
                panic!("catalog frame")
            };
            dispatcher.handle_now(
                WORKER_A,
                InboundFrame {
                    class: FrameClass::Rpc,
                    channel: 0,
                    frame: Up::RpiModelsListResult(proto::WRpiModelsListResult {
                        request_id: request.request_id,
                        worker_epoch: "process-a".into(),
                        error_code: "configuration".into(),
                        error_message: "secret-provider-token".into(),
                        ..Default::default()
                    }),
                },
            );
        }
    );
    let error = response.unwrap_err();
    assert_eq!(error.code, ErrorCode::FailedPrecondition);
    assert!(!error.message.unwrap().contains("secret-provider-token"));
}
