//! One ordered writer serializes browser inputs and worker output batches.
//! Admission and transcript updates commit together before downstream sends.
//! A failed worker batch fences cumulative ACKs until that batch is replayed.

use super::{super::sink::CoordChatSink, projection::OmpEffect};
use crate::{
    coord_core::worker_handle::{WorkerHandle, WorkerRegistry},
    db::CoordDb,
};
use roost_proto::{DOmpPrompt, WOmpEvents};
use roost_protocol::wire::{agent_chat::*, coord_worker::CoordWorkerDownstream};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};
use tokio::sync::{mpsc, oneshot};

pub struct OmpWriters {
    db: CoordDb,
    sink: Arc<CoordChatSink>,
    workers: Arc<WorkerRegistry>,
    senders: Mutex<HashMap<String, mpsc::UnboundedSender<WriterJob>>>,
}

pub(super) struct WriterState {
    pub db: CoordDb,
    pub sink: Arc<CoordChatSink>,
    pub workers: Arc<WorkerRegistry>,
}
impl std::fmt::Debug for OmpWriters {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("OmpWriters").finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum WriteError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error("OMP is busy")]
    Busy,
    #[error("worker offline")]
    Offline,
    #[error("OMP writer closed")]
    Closed,
}

pub(super) struct OmpInput {
    pub prompt: DOmpPrompt,
    pub attachment_names: Vec<String>,
    pub worker: Arc<WorkerHandle>,
    pub reject_busy: bool,
}

pub(super) enum WriterJob {
    Local {
        events: Vec<ChatEvent>,
        effects: Vec<OmpEffect>,
        done: oneshot::Sender<Result<u64, String>>,
    },
    Input {
        input: OmpInput,
        done: oneshot::Sender<Result<u64, WriteError>>,
    },
    Worker {
        frame: WOmpEvents,
        worker_fp: String,
    },
    Restart {
        worker_epoch: Option<String>,
    },
}

impl OmpWriters {
    pub fn new(db: CoordDb, sink: Arc<CoordChatSink>, workers: Arc<WorkerRegistry>) -> Self {
        Self {
            db,
            sink,
            workers,
            senders: Mutex::new(HashMap::new()),
        }
    }
    pub async fn local(
        &self,
        id: &str,
        events: Vec<ChatEvent>,
        effects: Vec<OmpEffect>,
    ) -> Result<u64, String> {
        let (done, result) = oneshot::channel();
        self.send(
            id,
            WriterJob::Local {
                events,
                effects,
                done,
            },
        );
        result.await.map_err(|error| error.to_string())?
    }
    pub(super) async fn submit(&self, id: &str, input: OmpInput) -> Result<u64, WriteError> {
        let (done, result) = oneshot::channel();
        self.send(id, WriterJob::Input { input, done });
        result.await.map_err(|_| WriteError::Closed)?
    }
    pub(super) fn send(&self, id: &str, job: WriterJob) {
        let mut senders = self.senders.lock().unwrap_or_else(PoisonError::into_inner);
        let job = match senders.get(id) {
            Some(sender) => match sender.send(job) {
                Ok(()) => return,
                Err(error) => error.0,
            },
            None => job,
        };
        let (sender, inbox) = mpsc::unbounded_channel();
        let _ = sender.send(job);
        let writer = WriterState {
            db: self.db.clone(),
            sink: Arc::clone(&self.sink),
            workers: Arc::clone(&self.workers),
        };
        let conversation_id = id.to_owned();
        tokio::spawn(async move {
            writer.write_jobs(&conversation_id, inbox).await;
        });
        senders.insert(id.to_owned(), sender);
    }
}

impl WriterState {
    async fn write_jobs(&self, id: &str, mut inbox: mpsc::UnboundedReceiver<WriterJob>) {
        let mut failed_batch: Option<(String, u64)> = None;
        while let Some(job) = inbox.recv().await {
            if let WriterJob::Worker { frame, .. } = &job
                && failed_batch
                    .as_ref()
                    .is_some_and(|(run, first)| run == &frame.run_id && frame.first_seq > *first)
                && !reports_output_loss(frame)
            {
                continue;
            }
            let result = self.persist(id, &job).await;
            if let Err(error) = &result {
                tracing::error!(conversation_id=id,%error,"omp event batch was not persisted");
            }
            match job {
                WriterJob::Local { done, .. } => {
                    let _ = done.send(
                        result
                            .map(|commit| commit.cursor)
                            .map_err(|error| error.to_string()),
                    );
                }
                WriterJob::Input { input, done } => {
                    let result = match result {
                        Ok(commit)
                            if commit.admitted
                                && input
                                    .worker
                                    .send(CoordWorkerDownstream::OmpPrompt(input.prompt))
                                    == 0 =>
                        {
                            let (ignored, _receiver) = oneshot::channel();
                            let failure = WriterJob::Local {
                                events: vec![
                                    super::projection::notice(
                                        "undelivered",
                                        commit.cursor,
                                        "error",
                                        "Message not delivered",
                                        "The worker went offline before OMP received this message.",
                                    ),
                                    ChatEvent::RunState {
                                        run_state: AgentRunState::Failed,
                                        error: Some("worker offline".into()),
                                    },
                                ],
                                effects: vec![OmpEffect::RunState(
                                    AgentRunState::Failed,
                                    Some("worker offline".into()),
                                )],
                                done: ignored,
                            };
                            match self.persist(id, &failure).await {
                                Ok(_) => Err(WriteError::Offline),
                                Err(error) => Err(error),
                            }
                        }
                        Ok(commit) => Ok(commit.cursor),
                        Err(error) => Err(error),
                    };
                    let _ = done.send(result);
                }
                WriterJob::Worker { frame, .. } => {
                    if result.is_err() {
                        failed_batch = Some((frame.run_id, frame.first_seq));
                    } else if result.as_ref().is_ok_and(|commit| commit.worker_applied)
                        && failed_batch.as_ref().is_none_or(|(run, first)| {
                            run != &frame.run_id
                                || frame
                                    .first_seq
                                    .saturating_add(frame.events_json.len() as u64)
                                    > *first
                        })
                    {
                        failed_batch = None;
                    }
                }
                WriterJob::Restart { .. } => {}
            }
        }
    }
}

pub(super) struct Commit {
    pub cursor: u64,
    pub admitted: bool,
    pub worker_applied: bool,
}

fn reports_output_loss(frame: &WOmpEvents) -> bool {
    frame.events_json.iter().any(|json| matches!(serde_json::from_str::<OmpWorkerEvent>(json),Ok(OmpWorkerEvent::Notice {title,..}) if title == "Some OMP output was lost"))
}
