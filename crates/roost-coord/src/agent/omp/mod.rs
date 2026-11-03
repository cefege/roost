//! Worker-hosted OMP conversations, durable projection, and scoped settings.
//! Browser commands and worker ACP frames converge through one ordered writer.
//! Native harness adoption and credentials never participate in this backend.

pub mod catalog;
pub mod projection;
mod projection_todo;
mod projection_tools;
pub mod rpc;
mod rpc_respond;
mod rpc_session;
pub mod rpc_settings;
mod settings;
pub mod store;
pub mod summary;
pub mod writer;
mod writer_effects;
mod writer_transaction;

use super::CoordChatSink;
use crate::{
    coord_core::{
        worker_handle::{WorkerHandle, WorkerRegistry},
        worker_lifecycle::WorkerLifecycleObserver,
    },
    db::CoordDb,
};
use roost_protocol::{
    versioning::{
        CAPABILITY_OMP_ACP_INTERACTIONS_V1, CAPABILITY_OMP_ACP_SESSION_V1, CAPABILITY_OMP_ACP_V1,
    },
    wire::{
        WorkerFp,
        coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream},
    },
};
use std::{collections::BTreeSet, sync::Arc};
use writer::{OmpWriters, WriterJob};

#[derive(Debug)]
pub struct OmpService {
    pub db: CoordDb,
    pub workers: Arc<WorkerRegistry>,
    pub sink: Arc<CoordChatSink>,
    pub writers: Arc<OmpWriters>,
}

#[derive(Debug, thiserror::Error)]
pub enum OmpUnavailable {
    #[error("worker offline")]
    Offline,
    #[error("Update Roost on this machine to use OMP chats")]
    UpdateRequired,
}

impl OmpService {
    pub fn new(db: CoordDb, workers: Arc<WorkerRegistry>, sink: Arc<CoordChatSink>) -> Self {
        Self {
            writers: Arc::new(OmpWriters::new(
                db.clone(),
                Arc::clone(&sink),
                Arc::clone(&workers),
            )),
            db,
            workers,
            sink,
        }
    }
    pub fn receive(&self, worker_fp: &str, upstream: CoordWorkerUpstream) -> bool {
        let CoordWorkerUpstream::OmpEvents(frame) = upstream else {
            return false;
        };
        let id = frame.conversation_id.clone();
        self.writers.send(
            &id,
            WriterJob::Worker {
                frame,
                worker_fp: worker_fp.into(),
            },
        );
        true
    }
    pub fn worker(&self, worker_fp: &str) -> Result<Arc<WorkerHandle>, OmpUnavailable> {
        self.interaction_worker(worker_fp)
    }
    pub fn worker_for_session(&self, worker_fp: &str) -> Result<Arc<WorkerHandle>, OmpUnavailable> {
        self.interaction_worker(worker_fp)
    }
    pub fn interaction_worker(&self, worker_fp: &str) -> Result<Arc<WorkerHandle>, OmpUnavailable> {
        let worker = self.routable_worker(worker_fp)?;
        if worker.capabilities.contains(CAPABILITY_OMP_ACP_V1)
            && worker.capabilities.contains(CAPABILITY_OMP_ACP_SESSION_V1)
            && worker
                .capabilities
                .contains(CAPABILITY_OMP_ACP_INTERACTIONS_V1)
        {
            Ok(worker)
        } else {
            Err(OmpUnavailable::UpdateRequired)
        }
    }
    fn routable_worker(&self, worker_fp: &str) -> Result<Arc<WorkerHandle>, OmpUnavailable> {
        let fp = WorkerFp::try_from(worker_fp.to_owned()).map_err(|_| OmpUnavailable::Offline)?;
        self.workers
            .current_routable(&fp)
            .ok_or(OmpUnavailable::Offline)
    }
    pub fn send_to_worker(
        &self,
        worker_fp: &str,
        frame: CoordWorkerDownstream,
    ) -> Result<(), OmpUnavailable> {
        if self.worker(worker_fp)?.send(frame) == 0 {
            Err(OmpUnavailable::Offline)
        } else {
            Ok(())
        }
    }
}

impl WorkerLifecycleObserver for OmpService {
    fn acknowledge_capabilities(&self, advertised: &BTreeSet<String>) -> Vec<&'static str> {
        let mut acknowledged = Vec::new();
        if advertised.contains(CAPABILITY_OMP_ACP_V1) {
            acknowledged.push(CAPABILITY_OMP_ACP_V1);
        }
        if advertised.contains(CAPABILITY_OMP_ACP_SESSION_V1) {
            acknowledged.push(CAPABILITY_OMP_ACP_SESSION_V1);
        }
        if advertised.contains(CAPABILITY_OMP_ACP_INTERACTIONS_V1) {
            acknowledged.push(CAPABILITY_OMP_ACP_INTERACTIONS_V1);
        }
        acknowledged
    }
    fn on_ready(&self, handle: &Arc<WorkerHandle>) {
        let db = self.db.clone();
        let writers = self.writers.clone();
        let worker = handle.worker_fp.to_string();
        let epoch = handle.process_epoch.clone();
        tokio::spawn(async move {
            match store::list(&db).await {
                Ok(conversations) => {
                    for conversation in conversations {
                        if conversation.worker_fp == worker && conversation.worker_epoch != epoch {
                            writers.send(
                                &conversation.id,
                                WriterJob::Restart {
                                    worker_epoch: epoch.clone(),
                                },
                            );
                        }
                    }
                }
                Err(error) => {
                    tracing::error!(%error,worker_fp=worker,"OMP restart reconciliation failed")
                }
            }
        });
    }
}
