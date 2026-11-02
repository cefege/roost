//! Worker-hosted OMP conversations, durable projection, and scoped settings.
//! Browser commands and worker ACP frames converge through one ordered writer.
//! Native harness adoption and credentials never participate in this backend.

pub mod catalog;
pub mod projection;
mod projection_tools;
pub mod rpc;
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
    versioning::CAPABILITY_OMP_ACP_V1,
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
#[error("worker offline")]
pub struct OmpUnavailable;

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
        let fp = WorkerFp::try_from(worker_fp.to_owned()).map_err(|_| OmpUnavailable)?;
        self.workers
            .current_routable(&fp)
            .filter(|worker| worker.capabilities.contains(CAPABILITY_OMP_ACP_V1))
            .ok_or(OmpUnavailable)
    }
    pub fn send_to_worker(
        &self,
        worker_fp: &str,
        frame: CoordWorkerDownstream,
    ) -> Result<(), OmpUnavailable> {
        if self.worker(worker_fp)?.send(frame) == 0 {
            Err(OmpUnavailable)
        } else {
            Ok(())
        }
    }
}

impl WorkerLifecycleObserver for OmpService {
    fn acknowledge_capabilities(&self, advertised: &BTreeSet<String>) -> Vec<&'static str> {
        if advertised.contains(CAPABILITY_OMP_ACP_V1) {
            vec![CAPABILITY_OMP_ACP_V1]
        } else {
            vec![]
        }
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
                        if conversation.worker_fp == worker
                            && conversation.run_state
                                == roost_protocol::wire::agent_chat::AgentRunState::Running
                            && conversation.worker_epoch != epoch
                        {
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
