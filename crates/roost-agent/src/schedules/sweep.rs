//! Delivery: submits every due schedule, then advances or removes its row.
//! The deterministic `sched-<id>-<due_ms>` request id makes a replayed
//! submit a no-op at the admission layer, so at-least-once sweeping plus
//! that dedup is exactly-once delivery.

use roost_protocol::wire::agent_chat::ChatEvent;

use super::to_wire;
use crate::error::AgentError;
use crate::runtime::{AgentRuntime, SubmitMode, now_ms};

/// Delivers every due schedule once; returns how many fired. Callable from
/// tests directly, without a real sweep interval.
pub async fn deliver_due(runtime: &AgentRuntime) -> Result<usize, AgentError> {
    let due = runtime.inner.store.due_schedules(now_ms()).await?;
    let mut fired = 0_usize;
    for (conversation_id, row) in due {
        let request_id = format!("sched-{}-{}", row.id, row.due_ms);
        if let Err(error) = runtime
            .submit_request_with_mode(
                &conversation_id,
                &request_id,
                row.text.clone(),
                SubmitMode::FollowUp,
            )
            .await
        {
            tracing::warn!(
                conversation_id = %conversation_id,
                schedule_id = %row.id,
                %error,
                "scheduled message delivery failed; retrying next sweep"
            );
            continue;
        }
        if advance_and_announce(runtime, &conversation_id, &row.id, row.due_ms).await {
            fired += 1;
        } else {
            continue;
        }
        tracing::info!(
            conversation_id = %conversation_id,
            schedule_id = %row.id,
            due_ms = row.due_ms,
            "scheduled message delivered"
        );
    }
    Ok(fired)
}

async fn advance_and_announce(
    runtime: &AgentRuntime,
    conversation_id: &str,
    schedule_id: &str,
    due_ms: i64,
) -> bool {
    match runtime
        .inner
        .store
        .advance_schedule(conversation_id, schedule_id, due_ms)
        .await
    {
        Ok(true) => {}
        Ok(false) => return false, // another replica already advanced this row
        Err(error) => {
            tracing::warn!(
                conversation_id = %conversation_id,
                schedule_id = %schedule_id,
                %error,
                "scheduled message advance failed after delivery"
            );
            return false;
        }
    }
    match runtime.inner.store.schedules(conversation_id).await {
        Ok(rows) => match rows.into_iter().find(|row| row.id == schedule_id) {
            Some(advanced) => {
                runtime
                    .emit(
                        conversation_id,
                        vec![ChatEvent::ScheduleSet {
                            schedule: to_wire(&advanced),
                        }],
                    )
                    .await;
            }
            None => {
                runtime
                    .emit(
                        conversation_id,
                        vec![ChatEvent::ScheduleRemoved {
                            id: schedule_id.to_owned(),
                        }],
                    )
                    .await;
            }
        },
        Err(error) => tracing::warn!(
            conversation_id = %conversation_id,
            schedule_id = %schedule_id,
            %error,
            "failed to re-read schedule after advance"
        ),
    }
    true
}
