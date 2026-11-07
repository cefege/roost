//! Atomic OMP transcript, input admission and worker sequence transactions.
//! The per-conversation writer invokes this path in command arrival order.
//! Committed inputs reach the worker before another input can overtake them.

use super::super::chat_events;
use super::{
    projection::{self, OmpEffect},
    store,
    summary::summary,
    writer::{Commit, WriteError, WriterJob, WriterState},
};
use crate::agent::sink::ChatSink;
use roost_protocol::wire::{WorkerFp, agent_chat::*, coord_worker::CoordWorkerDownstream};
use sqlx::Row;

impl WriterState {
    pub(super) async fn persist(&self, id: &str, job: &WriterJob) -> Result<Commit, WriteError> {
        let mut transaction = chat_events::begin_chat_write(&self.db).await?;
        if self.db.backend() == crate::db::DbBackend::Postgres {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('chat:' || $1, 0))")
                .bind(id)
                .execute(&mut *transaction)
                .await?;
        }
        let Some(row) = sqlx::query("SELECT s.*,c.worker_fp,c.run_state FROM omp_sessions s JOIN agent_conversations c ON c.id=s.conversation_id WHERE c.id=$1 AND c.backend='omp'")
            .bind(id).fetch_optional(&mut *transaction).await? else { return Err(sqlx::Error::RowNotFound.into()); };
        let worker_fp: String = row.try_get("worker_fp")?;
        let json: String = row.try_get("transcript_json")?;
        let mut transcript: Transcript =
            serde_json::from_str(&json).map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
        let mut cursor = row.try_get::<i64, _>("transcript_cursor")? as u64;
        let mut run_id: Option<String> = row.try_get("run_id")?;
        let mut applied_seq = row.try_get::<i64, _>("applied_seq")? as u64;
        let mut events = vec![];
        let mut effects = vec![];
        match job {
            WriterJob::Local {
                events: local,
                effects: local_effects,
                ..
            } => {
                append_events(&mut transcript, &mut events, local.iter().cloned());
                effects.extend(local_effects.iter().cloned());
            }
            WriterJob::Input { input, .. } => {
                if !input.worker.is_routable() {
                    return Err(WriteError::Offline);
                }
                if input.reject_busy && transcript.run_state == AgentRunState::Running {
                    return Err(WriteError::Busy);
                }
                let inserted = sqlx::query("INSERT INTO omp_inputs(conversation_id,request_id,kind,created_ms) VALUES($1,$2,'prompt',$3) ON CONFLICT DO NOTHING")
                    .bind(id).bind(&input.prompt.request_id).bind(super::super::now_ms()).execute(&mut *transaction).await?.rows_affected();
                if inserted == 0 {
                    return Ok(Commit {
                        cursor,
                        admitted: false,
                        worker_applied: false,
                    });
                }
                append_events(
                    &mut transcript,
                    &mut events,
                    [
                        ChatEvent::Item {
                            item: TranscriptItem::User {
                                id: format!("u-{}", input.prompt.request_id),
                                text: input.prompt.text.clone(),
                                attachment_names: input.attachment_names.clone(),
                            },
                        },
                        ChatEvent::RunState {
                            run_state: AgentRunState::Running,
                            error: None,
                        },
                    ],
                );
                effects.extend([
                    OmpEffect::RunState(AgentRunState::Running, None),
                    OmpEffect::WorkerEpoch(input.worker.process_epoch.clone()),
                ]);
            }
            WriterJob::Worker {
                frame,
                worker_fp: sender,
            } => {
                if sender != &worker_fp {
                    tracing::warn!(
                        conversation_id = id,
                        worker_fp = sender,
                        "omp events from a worker that does not own the conversation"
                    );
                    return Ok(Commit {
                        cursor,
                        admitted: false,
                        worker_applied: false,
                    });
                }
                if run_id.as_deref() != Some(&frame.run_id) {
                    let resolved = pending_interaction_resolutions(&transcript);
                    append_events(&mut transcript, &mut events, resolved);
                    run_id = Some(frame.run_id.clone());
                    applied_seq = 0;
                }
                for (idx, json) in frame.events_json.iter().enumerate() {
                    let seq = frame.first_seq.saturating_add(idx as u64);
                    if seq <= applied_seq {
                        continue;
                    }
                    match serde_json::from_str::<OmpWorkerEvent>(json) {
                        Ok(event) => {
                            let projected =
                                projection::project(&transcript, &frame.run_id, seq, &event);
                            append_events(&mut transcript, &mut events, projected.events);
                            effects.extend(projected.effects);
                        }
                        Err(error) => {
                            tracing::warn!(conversation_id=id,%error,"invalid OMP worker event ignored")
                        }
                    }
                    applied_seq = seq;
                }
            }
            WriterJob::Restart { worker_epoch } => {
                let previous: Option<String> = row.try_get("worker_epoch")?;
                let pending = pending_interaction_resolutions(&transcript);
                let was_running = transcript.run_state == AgentRunState::Running;
                let ui_open = transcript.omp_ui_transport_id.is_some()
                    || transcript.omp_ui.is_some()
                    || transcript.omp_ui_error.is_some();
                if &previous == worker_epoch {
                    return Ok(Commit {
                        cursor,
                        admitted: false,
                        worker_applied: false,
                    });
                }
                if ui_open {
                    events.push(ChatEvent::OmpUiReset { transport_id: None });
                    effects.push(OmpEffect::UiTransport(None));
                }
                effects.push(OmpEffect::WorkerEpoch(worker_epoch.clone()));
                if was_running || !pending.is_empty() {
                    events.extend(pending);
                    if was_running {
                        projection::close_open_items(&transcript, &mut events);
                        events.push(projection::notice(
                            "restart",
                            super::super::now_ms() as u64,
                            "error",
                            "OMP stopped",
                            "The worker restarted during this turn.",
                        ));
                        events.push(ChatEvent::RunState {
                            run_state: AgentRunState::Failed,
                            error: Some("worker restarted".into()),
                        });
                        effects.push(OmpEffect::RunState(
                            AgentRunState::Failed,
                            Some("worker restarted".into()),
                        ));
                    }
                }
                for event in &events {
                    fold_chat_event(&mut transcript, event);
                }
            }
        }
        let events_json = if events.is_empty() {
            None
        } else {
            Some(store::encode(&events)?)
        };
        if let Some(json) = &events_json {
            let run_start = events.iter().any(|event| {
                matches!(
                    event,
                    ChatEvent::RunState {
                        run_state: AgentRunState::Running,
                        ..
                    }
                )
            });
            cursor =
                chat_events::append_in(&mut transaction, self.db.backend(), id, json, run_start)
                    .await?;
        }
        for effect in &effects {
            super::writer_effects::apply(&mut transaction, id, &worker_fp, effect).await?;
        }
        sqlx::query("UPDATE omp_sessions SET transcript_json=$2,transcript_cursor=$3,run_id=$4,applied_seq=$5 WHERE conversation_id=$1")
            .bind(id).bind(store::encode(&transcript)?).bind(cursor as i64).bind(run_id).bind(applied_seq as i64).execute(&mut *transaction).await?;
        if !events.is_empty() || !effects.is_empty() {
            sqlx::query("UPDATE agent_conversations SET updated_ms=$2 WHERE id=$1")
                .bind(id)
                .bind(super::super::now_ms())
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        if let Some(json) = events_json {
            self.sink.publish_committed(id, cursor, json).await;
        }
        if !effects.is_empty() {
            match (
                store::find(&self.db, id).await,
                store::settings(&self.db).await,
            ) {
                (Ok(Some(conversation)), Ok(scopes)) => {
                    self.sink
                        .conversation(&summary(&conversation, &scopes))
                        .await
                }
                (Err(error), _) | (_, Err(error)) => {
                    tracing::error!(conversation_id=id,%error,"OMP committed summary publication failed")
                }
                _ => {}
            }
        }
        if let WriterJob::Worker { frame, .. } = job
            && let Ok(fp) = WorkerFp::try_from(worker_fp)
            && let Some(worker) = self.workers.current_routable(&fp)
        {
            worker.send(CoordWorkerDownstream::OmpEventsAck(
                roost_proto::DOmpEventsAck {
                    conversation_id: id.into(),
                    run_id: frame.run_id.clone(),
                    seq: frame
                        .first_seq
                        .saturating_add(frame.events_json.len() as u64)
                        .saturating_sub(1),
                    ..Default::default()
                },
            ));
        }
        Ok(Commit {
            cursor,
            admitted: matches!(job, WriterJob::Input { .. }),
            worker_applied: matches!(job, WriterJob::Worker { .. }),
        })
    }
}

fn append_events(
    transcript: &mut Transcript,
    events: &mut Vec<ChatEvent>,
    incoming: impl IntoIterator<Item = ChatEvent>,
) {
    for event in incoming {
        if let ChatEvent::Item { item } = &event
            && let Some(TranscriptItem::Assistant {
                id,
                streaming: true,
                ..
            }) = transcript.items.last()
            && !matches!(item, TranscriptItem::Assistant { id: next, .. } if next == id)
        {
            let mut closed = transcript.items.last().cloned();
            if let Some(TranscriptItem::Assistant { streaming, .. }) = closed.as_mut() {
                *streaming = false;
            }
            if let Some(item) = closed {
                let closed = ChatEvent::Item { item };
                fold_chat_event(transcript, &closed);
                events.push(closed);
            }
        }
        fold_chat_event(transcript, &event);
        events.push(event);
    }
}

fn pending_interaction_resolutions(transcript: &Transcript) -> Vec<ChatEvent> {
    transcript
        .omp_interactions
        .iter()
        .map(|interaction| ChatEvent::OmpInteractionResolved {
            id: interaction.id.clone(),
        })
        .collect()
}
