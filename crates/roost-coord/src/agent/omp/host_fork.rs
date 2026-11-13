//! Authenticated host-fork admission creates a distinct durable conversation.
//! Only a worker-issued child journal from the admitted parent view reaches here.
//! Unplaced children are removed on cancellation; placement commits ownership.

use super::super::{host_service_store::PendingHostService, store};
use crate::agent::sink::ChatSink;
use crate::coord_core::CoordCore;
use roost_proto as proto;
use roost_protocol::{
    native_fork::{ForkOpen, ForkTarget},
    wire::coord_worker::CoordWorkerDownstream,
};

fn child_id(service_id: &str) -> Option<String> {
    (!service_id.is_empty()
        && service_id.len() <= 96
        && service_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
    .then(|| format!("c-fork-{service_id}"))
}

pub(crate) async fn prepare(
    core: &CoordCore,
    parent: &store::OmpConversation,
    pending: &PendingHostService,
    value: &str,
) -> Result<String, sqlx::Error> {
    let open = ForkOpen::parse(value).ok_or(sqlx::Error::RowNotFound)?;
    if parent.acp_session_id.as_deref() != Some(&open.parent_session_id) {
        return Err(sqlx::Error::RowNotFound);
    }
    let id = child_id(&pending.service_id).ok_or(sqlx::Error::RowNotFound)?;
    let mut transaction = core.services.db.begin_write().await?;
    let lock = if core.services.db.backend() == crate::db::DbBackend::Postgres {
        "SELECT service_id FROM omp_host_services WHERE service_id=$1 AND conversation_id=$2 FOR UPDATE"
    } else {
        "SELECT service_id FROM omp_host_services WHERE service_id=$1 AND conversation_id=$2"
    };
    let _: String = sqlx::query_scalar(lock)
        .bind(&pending.service_id)
        .bind(&parent.id)
        .fetch_one(&mut *transaction)
        .await?;
    sqlx::query("INSERT INTO agent_conversations(id,title,worker_fp,worker_label,worker_os,cwd,model_provider,model_id,thinking_level,mode,parent_id,read_only,run_state,created_ms,updated_ms,revision,backend) SELECT $1,'',worker_fp,worker_label,worker_os,cwd,model_provider,model_id,thinking_level,mode,id,$3,'idle',$4,$4,1,'omp' FROM agent_conversations WHERE id=$2 AND backend='omp' ON CONFLICT DO NOTHING")
        .bind(&id).bind(&parent.id).bind(false).bind(crate::serve::now_ms())
        .execute(&mut *transaction).await?;
    sqlx::query("INSERT INTO omp_sessions(conversation_id,transcript_json,acp_session_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
        .bind(&id).bind(store::encode(&store::empty_transcript())?).bind(&open.session_id)
        .execute(&mut *transaction).await?;
    // The marker is removed only after actual placement, never after journal creation.
    sqlx::query("INSERT INTO omp_inputs(conversation_id,request_id,kind,created_ms,transport_id,expires_ms) VALUES($1,$2,'ui',$3,$4,$5) ON CONFLICT DO NOTHING")
        .bind(&id).bind(&pending.service_id).bind(crate::serve::now_ms())
        .bind(&pending.transport_id).bind(crate::serve::now_ms() + super::super::host_service_store::HOST_SERVICE_TTL_MS)
        .execute(&mut *transaction).await?;
    transaction.commit().await?;
    let child = store::find(&core.services.db, &id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    let scopes = store::settings(&core.services.db).await?;
    core.services
        .agent
        .sink
        .conversation(&super::super::summary::summary(&child, &scopes))
        .await;
    if core
        .services
        .omp
        .send_to_worker(
            &parent.worker_fp,
            CoordWorkerDownstream::OmpOpen(proto::DOmpOpen {
                conversation_id: id.clone(),
                cwd: parent.cwd.clone(),
                acp_session_id: open.session_id,
                ..Default::default()
            }),
        )
        .is_err()
    {
        finish(core, &parent.id, &pending.service_id, false).await?;
        return Err(sqlx::Error::RowNotFound);
    }
    tracing::info!(parent_conversation_id=%parent.id, conversation_id=%id, "OMP fork child bootstrap admitted");
    serde_json::to_string(&ForkTarget {
        conversation_id: id,
        parent_conversation_id: parent.id.clone(),
        placement: open.placement,
    })
    .map_err(|error| sqlx::Error::Encode(Box::new(error)))
}

pub(crate) async fn finish(
    core: &CoordCore,
    parent_id: &str,
    service_id: &str,
    accepted: bool,
) -> Result<(), sqlx::Error> {
    let Some(id) = child_id(service_id) else {
        return Ok(());
    };
    if accepted {
        sqlx::query(
            "DELETE FROM omp_inputs WHERE conversation_id=$1 AND request_id=$2 AND kind='ui'",
        )
        .bind(&id)
        .bind(service_id)
        .execute(core.services.db.pool())
        .await?;
        tracing::info!(parent_conversation_id=parent_id, conversation_id=%id, "OMP fork HTML placement confirmed");
        return Ok(());
    }
    let deleted = sqlx::query("DELETE FROM agent_conversations WHERE id=$1 AND parent_id=$2 AND backend='omp' AND EXISTS(SELECT 1 FROM omp_inputs WHERE conversation_id=$1 AND request_id=$3 AND kind='ui')")
        .bind(&id).bind(parent_id).bind(service_id).execute(core.services.db.pool()).await?.rows_affected();
    if deleted != 0 {
        if let Some(parent) = store::find(&core.services.db, parent_id).await? {
            let _ = core.services.omp.send_to_worker(
                &parent.worker_fp,
                CoordWorkerDownstream::OmpClose(proto::DOmpClose {
                    conversation_id: id.clone(),
                    ..Default::default()
                }),
            );
        }
        core.services.agent.sink.removed(&id).await;
        tracing::info!(parent_conversation_id=parent_id, conversation_id=%id, "OMP unplaced fork child removed");
    }
    Ok(())
}
