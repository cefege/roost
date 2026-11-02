//! SQL effects of an ACP projection, applied inside the chat batch transaction.
//! Auto-titles never overwrite an operator title; catalog changes stay machine-local.
//! The ordered writer publishes only after these updates commit.

use super::{projection::OmpEffect, store};

pub(super) async fn apply(
    transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
    id: &str,
    worker_fp: &str,
    effect: &OmpEffect,
) -> Result<(), sqlx::Error> {
    match effect {
        OmpEffect::SessionReady {
            acp_session_id,
            config_options,
        } => {
            sqlx::query("UPDATE omp_sessions SET acp_session_id=$2 WHERE conversation_id=$1")
                .bind(id)
                .bind(acp_session_id)
                .execute(&mut **transaction)
                .await?;
            config(transaction, id, worker_fp, config_options).await?;
        }
        OmpEffect::ConfigOptions(options) => config(transaction, id, worker_fp, options).await?,
        OmpEffect::Title(title) => {
            let changed = sqlx::query("UPDATE agent_conversations SET title=$2 WHERE id=$1 AND (title='' OR title=(SELECT applied_title FROM omp_sessions WHERE conversation_id=$1))")
                .bind(id).bind(title).execute(&mut **transaction).await?.rows_affected();
            if changed > 0 {
                sqlx::query("UPDATE omp_sessions SET applied_title=$2 WHERE conversation_id=$1")
                    .bind(id)
                    .bind(title)
                    .execute(&mut **transaction)
                    .await?;
            }
        }
        OmpEffect::RunState(state, error) => {
            sqlx::query("UPDATE agent_conversations SET run_state=$2,error=$3 WHERE id=$1")
                .bind(id)
                .bind(state.as_str())
                .bind(error)
                .execute(&mut **transaction)
                .await?;
            tracing::info!(
                conversation_id = id,
                run_state = state.as_str(),
                "OMP run state changed"
            );
        }
        OmpEffect::WorkerEpoch(epoch) => {
            sqlx::query("UPDATE omp_sessions SET worker_epoch=$2 WHERE conversation_id=$1")
                .bind(id)
                .bind(epoch)
                .execute(&mut **transaction)
                .await?;
        }
    }
    Ok(())
}

async fn config(
    transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
    id: &str,
    worker_fp: &str,
    options: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    let (model, thinking, _) = super::catalog::current_values(options);
    sqlx::query("UPDATE omp_sessions SET current_model=$2,current_thinking_level=$3 WHERE conversation_id=$1")
        .bind(id).bind(model).bind(thinking).execute(&mut **transaction).await?;
    store::put_catalog(transaction, worker_fp, options).await
}
